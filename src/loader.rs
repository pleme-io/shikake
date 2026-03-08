//! Plugin discovery and loading.
//!
//! Discovers cdylib plugin files from a configured directory, resolves their
//! load order from the dependency graph, and loads them into Neovim via
//! `require()`.

use std::path::{Path, PathBuf};

use nvim_oxi::api;

use crate::spec::{self, PluginSpec, ResolveError};
use crate::state;

/// Errors from the loader.
#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("dependency resolution failed: {0}")]
    Resolve(#[from] ResolveError),

    #[error("failed to load plugin '{name}': {reason}")]
    Load { name: String, reason: String },

    #[error("plugin directory does not exist: {0}")]
    DirNotFound(PathBuf),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Discover cdylib files (`.so`, `.dylib`, `.dll`) in the given directory.
/// Returns a list of `(stem, path)` pairs where stem is the file name without
/// the library prefix and extension (e.g. `libfoo.so` -> `foo`).
///
/// Only files at the top level of `dir` are considered (no recursion).
pub fn discover_plugins(dir: &Path) -> Result<Vec<(String, PathBuf)>, LoadError> {
    if !dir.is_dir() {
        return Err(LoadError::DirNotFound(dir.to_path_buf()));
    }

    let mut plugins = Vec::new();

    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();

        if !path.is_file() {
            continue;
        }

        let ext = path.extension().and_then(|e| e.to_str());
        let is_cdylib = matches!(ext, Some("so" | "dylib" | "dll"));
        if !is_cdylib {
            continue;
        }

        if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
            // Strip the `lib` prefix that Rust adds to cdylib outputs on
            // Unix-like systems.
            let name = stem.strip_prefix("lib").unwrap_or(stem);
            plugins.push((name.to_string(), path));
        }
    }

    plugins.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(plugins)
}

/// Build a Lua command string that adds a directory to `package.cpath` and
/// requires a module. Uses `:lua` ex-command prefix.
fn lua_require_command(name: &str, path: &Path) -> String {
    let dir = path
        .parent()
        .unwrap_or(Path::new("."))
        .to_string_lossy();

    // Determine the cpath glob pattern based on platform.
    #[cfg(target_os = "macos")]
    let ext = "dylib";
    #[cfg(target_os = "windows")]
    let ext = "dll";
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let ext = "so";

    format!(
        "lua do \
         local e = \"{dir}/?.{ext}\" \
         if not package.cpath:find(e, 1, true) then \
         package.cpath = package.cpath .. \";\" .. e end \
         require(\"{name}\") \
         end",
    )
}

/// Load a single plugin by calling Lua's `require()` on its module name
/// via Neovim's `:lua` ex-command.
///
/// Before calling require, the plugin's cdylib path is added to Lua's
/// `package.cpath` so Neovim can find it.
fn load_one(name: &str, path: &Path) -> Result<(), LoadError> {
    let cmd = lua_require_command(name, path);
    api::command(&cmd).map_err(|e| LoadError::Load {
        name: name.to_string(),
        reason: format!("{e}"),
    })?;
    Ok(())
}

/// Load all eager plugins from the given specs in dependency order.
///
/// Lazy plugins are registered as pending but not loaded — their triggers
/// will be set up separately.
pub fn load_eager(specs: &[PluginSpec]) -> Result<(), LoadError> {
    let order = spec::resolve_load_order(specs)?;

    let by_name: std::collections::HashMap<&str, &PluginSpec> =
        specs.iter().map(|s| (s.name.as_str(), s)).collect();

    let state = state::global();

    for name in &order {
        let Some(spec) = by_name.get(name.as_str()) else {
            continue;
        };

        {
            let mut s = state.lock().expect("state lock poisoned");
            s.register(name);
        }

        if spec.is_lazy() {
            // Leave as pending — triggers will be set up by the caller.
            continue;
        }

        let (result, duration) = state::timed(|| load_one(name, &spec.path));
        let mut s = state.lock().expect("state lock poisoned");
        match result {
            Ok(()) => s.mark_loaded(name, duration),
            Err(e) => s.mark_errored(name, e.to_string()),
        }
    }

    Ok(())
}

/// Load a single plugin by name (used when a lazy trigger fires).
pub fn load_lazy(spec: &PluginSpec) -> Result<(), LoadError> {
    let state = state::global();

    // Check if already loaded.
    {
        let s = state.lock().expect("state lock poisoned");
        if let Some(entry) = s.get(&spec.name) {
            if matches!(entry.status, state::PluginStatus::Loaded { .. }) {
                return Ok(());
            }
        }
    }

    let (result, duration) = state::timed(|| load_one(&spec.name, &spec.path));
    let mut s = state.lock().expect("state lock poisoned");
    match result {
        Ok(()) => {
            s.mark_loaded(&spec.name, duration);
            Ok(())
        }
        Err(e) => {
            let msg = e.to_string();
            s.mark_errored(&spec.name, &msg);
            Err(e)
        }
    }
}

/// Unload a plugin by clearing its Lua module from `package.loaded`.
/// This allows a subsequent `require()` to reload it from disk.
pub fn unload(name: &str) -> Result<(), LoadError> {
    let cmd = format!("lua package.loaded[\"{name}\"] = nil");
    api::command(&cmd).map_err(|e| LoadError::Load {
        name: name.to_string(),
        reason: format!("unload failed: {e}"),
    })?;

    let state = state::global();
    let mut s = state.lock().expect("state lock poisoned");
    s.reset(name);
    Ok(())
}

/// Format the status output for `:ShikakeStatus`.
#[must_use]
pub fn format_status() -> String {
    let state = state::global();
    let s = state.lock().expect("state lock poisoned");
    let counts = s.counts();
    let total = s.total_load_time();

    let mut lines = vec![format!(
        "Shikake: {} loaded, {} pending, {} errored (total: {total:.2?})",
        counts.loaded, counts.pending, counts.errored,
    )];

    for entry in s.all_sorted() {
        let icon = match &entry.status {
            state::PluginStatus::Loaded { .. } => "  *",
            state::PluginStatus::Pending => "  o",
            state::PluginStatus::Errored { .. } => "  x",
        };
        lines.push(format!("{icon} {} -- {}", entry.name, entry.status));
    }

    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discover_nonexistent_dir() {
        let err = discover_plugins(Path::new("/nonexistent/path/plugins"));
        assert!(err.is_err());
        match err.unwrap_err() {
            LoadError::DirNotFound(p) => {
                assert_eq!(p, PathBuf::from("/nonexistent/path/plugins"));
            }
            other => panic!("expected DirNotFound, got {other:?}"),
        }
    }

    #[test]
    fn discover_empty_dir() {
        let dir = std::env::temp_dir().join("shikake_test_empty");
        let _ = std::fs::create_dir_all(&dir);
        let plugins = discover_plugins(&dir).unwrap();
        assert!(plugins.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn discover_filters_non_cdylib() {
        let dir = std::env::temp_dir().join("shikake_test_filter");
        let _ = std::fs::create_dir_all(&dir);

        // Create test files.
        std::fs::write(dir.join("libfoo.so"), b"").unwrap();
        std::fs::write(dir.join("libbar.dylib"), b"").unwrap();
        std::fs::write(dir.join("not_a_plugin.txt"), b"").unwrap();
        std::fs::write(dir.join("readme.md"), b"").unwrap();

        let plugins = discover_plugins(&dir).unwrap();
        let names: Vec<&str> = plugins.iter().map(|(n, _)| n.as_str()).collect();
        assert!(names.contains(&"bar"));
        assert!(names.contains(&"foo"));
        assert!(!names.contains(&"not_a_plugin"));
        assert!(!names.contains(&"readme"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn discover_strips_lib_prefix() {
        let dir = std::env::temp_dir().join("shikake_test_prefix");
        let _ = std::fs::create_dir_all(&dir);

        std::fs::write(dir.join("libmyplugin.so"), b"").unwrap();
        std::fs::write(dir.join("noprefixplugin.so"), b"").unwrap();

        let plugins = discover_plugins(&dir).unwrap();
        let names: Vec<&str> = plugins.iter().map(|(n, _)| n.as_str()).collect();
        assert!(names.contains(&"myplugin"));
        assert!(names.contains(&"noprefixplugin"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn format_status_output() {
        // Just verify it doesn't panic and produces non-empty output.
        let output = format_status();
        assert!(output.contains("Shikake:"));
    }

    #[test]
    fn lua_require_command_format() {
        let cmd = lua_require_command("myplugin", Path::new("/opt/plugins/libmyplugin.so"));
        assert!(cmd.contains("require(\"myplugin\")"));
        assert!(cmd.contains("/opt/plugins"));
    }
}
