//! Plugin specification and dependency resolution.
//!
//! A [`PluginSpec`] describes a single plugin: its name, path on disk, lazy
//! loading triggers, dependencies, and enabled state. The [`resolve_load_order`]
//! function performs a topological sort over a set of specs to determine the
//! correct loading sequence.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;

/// Describes when a plugin should be lazily loaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LazyTrigger {
    /// Load on a Neovim event, optionally scoped to a file pattern.
    /// Examples: `Event("BufEnter", None)`, `Event("FileType", Some("rust"))`.
    Event(String, Option<String>),

    /// Load when a user command is first invoked.
    Command(String),

    /// Load when a filetype is detected.
    Filetype(String),

    /// Load when a keymap is pressed (mode, lhs).
    Keymap(String, String),
}

/// Full specification for a single plugin.
#[derive(Debug, Clone)]
pub struct PluginSpec {
    /// Unique plugin name (typically the crate name, e.g. `"sakuin"`).
    pub name: String,

    /// Path to the compiled cdylib file.
    pub path: PathBuf,

    /// If empty, the plugin loads eagerly at startup.
    pub lazy: Vec<LazyTrigger>,

    /// Names of plugins that must be loaded before this one.
    pub dependencies: Vec<String>,

    /// Whether this plugin is enabled. Disabled plugins are skipped entirely.
    pub enabled: bool,
}

impl PluginSpec {
    /// Create a new plugin spec with the given name and path.
    /// Defaults to eager loading, no dependencies, and enabled.
    #[must_use]
    pub fn new(name: impl Into<String>, path: impl Into<PathBuf>) -> Self {
        Self {
            name: name.into(),
            path: path.into(),
            lazy: Vec::new(),
            dependencies: Vec::new(),
            enabled: true,
        }
    }

    /// Add a lazy trigger.
    #[must_use]
    pub fn on(mut self, trigger: LazyTrigger) -> Self {
        self.lazy.push(trigger);
        self
    }

    /// Add a dependency by name.
    #[must_use]
    pub fn depends_on(mut self, dep: impl Into<String>) -> Self {
        self.dependencies.push(dep.into());
        self
    }

    /// Disable this plugin.
    #[must_use]
    pub fn disabled(mut self) -> Self {
        self.enabled = false;
        self
    }

    /// Whether this plugin should load lazily.
    #[must_use]
    pub fn is_lazy(&self) -> bool {
        !self.lazy.is_empty()
    }

    /// Whether this plugin should load eagerly (at startup).
    #[must_use]
    pub fn is_eager(&self) -> bool {
        self.lazy.is_empty()
    }

    /// Check whether a given event (name + optional pattern) matches any of
    /// this plugin's lazy triggers.
    #[must_use]
    pub fn matches_event(&self, event: &str, pattern: Option<&str>) -> bool {
        self.lazy.iter().any(|t| match t {
            LazyTrigger::Event(ev, pat) => {
                ev == event
                    && match (pat.as_deref(), pattern) {
                        (None, _) => true,
                        (Some(a), Some(b)) => a == b,
                        (Some(_), None) => false,
                    }
            }
            _ => false,
        })
    }

    /// Check whether a command name matches any of this plugin's lazy triggers.
    #[must_use]
    pub fn matches_command(&self, cmd: &str) -> bool {
        self.lazy.iter().any(|t| matches!(t, LazyTrigger::Command(c) if c == cmd))
    }

    /// Check whether a filetype matches any of this plugin's lazy triggers.
    #[must_use]
    pub fn matches_filetype(&self, ft: &str) -> bool {
        self.lazy.iter().any(|t| matches!(t, LazyTrigger::Filetype(f) if f == ft))
    }

    /// Check whether a keymap (mode, lhs) matches any of this plugin's lazy triggers.
    #[must_use]
    pub fn matches_keymap(&self, mode: &str, lhs: &str) -> bool {
        self.lazy
            .iter()
            .any(|t| matches!(t, LazyTrigger::Keymap(m, l) if m == mode && l == lhs))
    }
}

/// Errors that can occur during dependency resolution.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResolveError {
    /// A plugin declares a dependency that is not in the spec set.
    #[error("plugin '{from}' depends on unknown plugin '{dep}'")]
    MissingDependency { from: String, dep: String },

    /// A cycle exists in the dependency graph.
    #[error("dependency cycle detected involving: {cycle}")]
    Cycle { cycle: String },
}

/// Resolve the load order for a set of plugin specs using topological sort
/// (Kahn's algorithm). Returns the plugin names in an order that satisfies
/// all dependency constraints.
///
/// Disabled plugins and their dependents are excluded from the result.
///
/// # Errors
///
/// Returns [`ResolveError::MissingDependency`] if a plugin references a
/// dependency that is not present in `specs`.
///
/// Returns [`ResolveError::Cycle`] if the dependency graph contains a cycle.
pub fn resolve_load_order(specs: &[PluginSpec]) -> Result<Vec<String>, ResolveError> {
    // Index enabled specs by name.
    let enabled: HashMap<&str, &PluginSpec> = specs
        .iter()
        .filter(|s| s.enabled)
        .map(|s| (s.name.as_str(), s))
        .collect();

    // Validate all declared dependencies exist.
    for spec in enabled.values() {
        for dep in &spec.dependencies {
            if !enabled.contains_key(dep.as_str()) {
                return Err(ResolveError::MissingDependency {
                    from: spec.name.clone(),
                    dep: dep.clone(),
                });
            }
        }
    }

    // Build adjacency list and in-degree map.
    let mut in_degree: HashMap<&str, usize> = HashMap::new();
    let mut dependents: HashMap<&str, Vec<&str>> = HashMap::new();

    for name in enabled.keys() {
        in_degree.entry(name).or_insert(0);
        dependents.entry(name).or_default();
    }

    for spec in enabled.values() {
        for dep in &spec.dependencies {
            // dep -> spec.name (dep must load before spec)
            dependents.entry(dep.as_str()).or_default().push(&spec.name);
            *in_degree.entry(spec.name.as_str()).or_insert(0) += 1;
        }
    }

    // Kahn's algorithm.
    let mut queue: VecDeque<&str> = in_degree
        .iter()
        .filter(|&(_, &deg)| deg == 0)
        .map(|(&name, _)| name)
        .collect();

    // Sort the initial queue for deterministic output.
    let mut sorted_queue: Vec<&str> = queue.drain(..).collect();
    sorted_queue.sort_unstable();
    queue.extend(sorted_queue);

    let mut order: Vec<String> = Vec::with_capacity(enabled.len());

    while let Some(name) = queue.pop_front() {
        order.push(name.to_string());

        if let Some(deps) = dependents.get(name) {
            let mut next: Vec<&str> = Vec::new();
            for &dependent in deps {
                let deg = in_degree.get_mut(dependent).expect("in_degree missing");
                *deg -= 1;
                if *deg == 0 {
                    next.push(dependent);
                }
            }
            // Sort for determinism.
            next.sort_unstable();
            queue.extend(next);
        }
    }

    if order.len() != enabled.len() {
        // Find the cycle participants (nodes still with in_degree > 0).
        let mut cycle_members: Vec<&str> = in_degree
            .iter()
            .filter(|&(_, &deg)| deg > 0)
            .map(|(&name, _)| name)
            .collect();
        cycle_members.sort_unstable();
        return Err(ResolveError::Cycle {
            cycle: cycle_members.join(", "),
        });
    }

    Ok(order)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(name: &str) -> PluginSpec {
        PluginSpec::new(name, format!("/plugins/{name}.so"))
    }

    // ── PluginSpec basics ───────────────────────────────────────────

    #[test]
    fn new_spec_is_eager_and_enabled() {
        let s = spec("foo");
        assert!(s.is_eager());
        assert!(!s.is_lazy());
        assert!(s.enabled);
    }

    #[test]
    fn adding_trigger_makes_lazy() {
        let s = spec("foo").on(LazyTrigger::Command("Foo".into()));
        assert!(s.is_lazy());
        assert!(!s.is_eager());
    }

    #[test]
    fn disabled_spec() {
        let s = spec("foo").disabled();
        assert!(!s.enabled);
    }

    // ── Trigger matching ────────────────────────────────────────────

    #[test]
    fn matches_event_no_pattern() {
        let s = spec("foo").on(LazyTrigger::Event("BufEnter".into(), None));
        assert!(s.matches_event("BufEnter", None));
        assert!(s.matches_event("BufEnter", Some("*.rs")));
        assert!(!s.matches_event("BufLeave", None));
    }

    #[test]
    fn matches_event_with_pattern() {
        let s = spec("foo").on(LazyTrigger::Event(
            "FileType".into(),
            Some("rust".into()),
        ));
        assert!(s.matches_event("FileType", Some("rust")));
        assert!(!s.matches_event("FileType", Some("lua")));
        assert!(!s.matches_event("FileType", None));
    }

    #[test]
    fn matches_command() {
        let s = spec("foo").on(LazyTrigger::Command("FooRun".into()));
        assert!(s.matches_command("FooRun"));
        assert!(!s.matches_command("BarRun"));
    }

    #[test]
    fn matches_filetype() {
        let s = spec("foo").on(LazyTrigger::Filetype("rust".into()));
        assert!(s.matches_filetype("rust"));
        assert!(!s.matches_filetype("python"));
    }

    #[test]
    fn matches_keymap() {
        let s = spec("foo").on(LazyTrigger::Keymap("n".into(), "<leader>f".into()));
        assert!(s.matches_keymap("n", "<leader>f"));
        assert!(!s.matches_keymap("i", "<leader>f"));
        assert!(!s.matches_keymap("n", "<leader>g"));
    }

    #[test]
    fn multiple_triggers() {
        let s = spec("foo")
            .on(LazyTrigger::Command("Foo".into()))
            .on(LazyTrigger::Filetype("rust".into()))
            .on(LazyTrigger::Keymap("n".into(), "<leader>f".into()));

        assert!(s.matches_command("Foo"));
        assert!(s.matches_filetype("rust"));
        assert!(s.matches_keymap("n", "<leader>f"));
        assert!(!s.matches_event("BufEnter", None));
    }

    // ── Dependency resolution ───────────────────────────────────────

    #[test]
    fn empty_specs() {
        let order = resolve_load_order(&[]).unwrap();
        assert!(order.is_empty());
    }

    #[test]
    fn single_plugin() {
        let order = resolve_load_order(&[spec("alpha")]).unwrap();
        assert_eq!(order, vec!["alpha"]);
    }

    #[test]
    fn independent_plugins_sorted_alphabetically() {
        let specs = vec![spec("charlie"), spec("alpha"), spec("bravo")];
        let order = resolve_load_order(&specs).unwrap();
        assert_eq!(order, vec!["alpha", "bravo", "charlie"]);
    }

    #[test]
    fn linear_dependency_chain() {
        let specs = vec![
            spec("c").depends_on("b"),
            spec("b").depends_on("a"),
            spec("a"),
        ];
        let order = resolve_load_order(&specs).unwrap();
        assert_eq!(order, vec!["a", "b", "c"]);
    }

    #[test]
    fn diamond_dependency() {
        // d depends on b and c, both depend on a.
        let specs = vec![
            spec("d").depends_on("b").depends_on("c"),
            spec("b").depends_on("a"),
            spec("c").depends_on("a"),
            spec("a"),
        ];
        let order = resolve_load_order(&specs).unwrap();

        // a must come first, d must come last, b and c in between (alphabetical).
        assert_eq!(order[0], "a");
        assert_eq!(order[3], "d");
        assert!(order[1..3].contains(&"b".to_string()));
        assert!(order[1..3].contains(&"c".to_string()));
    }

    #[test]
    fn disabled_plugins_excluded() {
        let specs = vec![
            spec("b").depends_on("a"),
            spec("a"),
            spec("c").disabled(),
        ];
        let order = resolve_load_order(&specs).unwrap();
        assert_eq!(order, vec!["a", "b"]);
        assert!(!order.contains(&"c".to_string()));
    }

    #[test]
    fn disabled_dependency_is_missing() {
        let specs = vec![spec("b").depends_on("a"), spec("a").disabled()];
        let err = resolve_load_order(&specs).unwrap_err();
        assert_eq!(
            err,
            ResolveError::MissingDependency {
                from: "b".into(),
                dep: "a".into(),
            }
        );
    }

    #[test]
    fn missing_dependency_error() {
        let specs = vec![spec("a").depends_on("nonexistent")];
        let err = resolve_load_order(&specs).unwrap_err();
        assert_eq!(
            err,
            ResolveError::MissingDependency {
                from: "a".into(),
                dep: "nonexistent".into(),
            }
        );
    }

    #[test]
    fn cycle_detected() {
        let specs = vec![spec("a").depends_on("b"), spec("b").depends_on("a")];
        let err = resolve_load_order(&specs).unwrap_err();
        match err {
            ResolveError::Cycle { cycle } => {
                assert!(cycle.contains("a"));
                assert!(cycle.contains("b"));
            }
            other => panic!("expected Cycle, got {other:?}"),
        }
    }

    #[test]
    fn three_way_cycle() {
        let specs = vec![
            spec("a").depends_on("c"),
            spec("b").depends_on("a"),
            spec("c").depends_on("b"),
        ];
        let err = resolve_load_order(&specs).unwrap_err();
        match err {
            ResolveError::Cycle { cycle } => {
                assert!(cycle.contains("a"));
                assert!(cycle.contains("b"));
                assert!(cycle.contains("c"));
            }
            other => panic!("expected Cycle, got {other:?}"),
        }
    }

    #[test]
    fn partial_cycle_still_detected() {
        // d is fine, but a->b->c->a is a cycle.
        let specs = vec![
            spec("d"),
            spec("a").depends_on("c"),
            spec("b").depends_on("a"),
            spec("c").depends_on("b"),
        ];
        let err = resolve_load_order(&specs).unwrap_err();
        match err {
            ResolveError::Cycle { cycle } => {
                assert!(cycle.contains("a"));
                assert!(!cycle.contains("d"));
            }
            other => panic!("expected Cycle, got {other:?}"),
        }
    }

    #[test]
    fn complex_graph() {
        // e -> c, d
        // d -> b
        // c -> a, b
        // b -> a
        // a (root)
        let specs = vec![
            spec("e").depends_on("c").depends_on("d"),
            spec("d").depends_on("b"),
            spec("c").depends_on("a").depends_on("b"),
            spec("b").depends_on("a"),
            spec("a"),
        ];
        let order = resolve_load_order(&specs).unwrap();

        // Verify constraints.
        let pos = |name: &str| order.iter().position(|n| n == name).unwrap();
        assert!(pos("a") < pos("b"));
        assert!(pos("a") < pos("c"));
        assert!(pos("b") < pos("c"));
        assert!(pos("b") < pos("d"));
        assert!(pos("c") < pos("e"));
        assert!(pos("d") < pos("e"));
    }

    #[test]
    fn duplicate_dependency_is_harmless() {
        let specs = vec![
            spec("b").depends_on("a").depends_on("a"),
            spec("a"),
        ];
        let order = resolve_load_order(&specs).unwrap();
        assert_eq!(order, vec!["a", "b"]);
    }

    // ── Builder chaining ────────────────────────────────────────────

    #[test]
    fn builder_chaining() {
        let s = PluginSpec::new("test", "/lib/test.so")
            .on(LazyTrigger::Command("TestCmd".into()))
            .on(LazyTrigger::Filetype("rust".into()))
            .depends_on("dep_a")
            .depends_on("dep_b");

        assert_eq!(s.name, "test");
        assert_eq!(s.lazy.len(), 2);
        assert_eq!(s.dependencies, vec!["dep_a", "dep_b"]);
        assert!(s.enabled);
        assert!(s.is_lazy());
    }
}
