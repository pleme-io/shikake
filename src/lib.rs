//! Shikake (仕掛け) — Rust-native Neovim plugin loader and lifecycle manager.
//!
//! A plugin loader for blnvim-ng that discovers compiled Rust cdylib plugins
//! from a configured directory and loads them into Neovim. Supports lazy
//! loading on events, commands, filetypes, and keymaps.
//!
//! # Commands
//!
//! - `:ShikakeStatus` — show which plugins are loaded, pending, or errored.
//! - `:ShikakeReload <name>` — unload and reload a plugin (for development).
//!
//! Part of the blnvim-ng distribution — a Rust-native Neovim plugin suite.
//! Built with [`nvim-oxi`](https://github.com/noib3/nvim-oxi) for zero-cost
//! Neovim API bindings.

pub mod loader;
pub mod spec;
pub mod state;

use nvim_oxi as oxi;
use nvim_oxi::api;
use nvim_oxi::api::opts::EchoOpts;
use tane::prelude::*;

/// Convert a `tane::Error` into an `oxi::Error`.
fn tane_err(e: tane::Error) -> oxi::Error {
    oxi::Error::from(oxi::api::Error::Other(e.to_string()))
}

#[oxi::plugin]
fn shikake() -> oxi::Result<()> {
    let echo_opts = EchoOpts::builder().build();

    // Register :ShikakeStatus command.
    UserCommand::new("ShikakeStatus")
        .desc("Show shikake plugin loader status")
        .register(|_args| {
            let output = loader::format_status();
            let opts = EchoOpts::builder().build();
            let _ = api::echo([(output.as_str(), None::<&str>)], true, &opts);
            Ok(())
        })
        .map_err(tane_err)?;

    // Register :ShikakeReload <name> command.
    UserCommand::new("ShikakeReload")
        .desc("Unload and reload a plugin by name")
        .one_arg()
        .register(|args| {
            let name = args
                .args
                .as_deref()
                .unwrap_or("")
                .trim()
                .to_string();

            if name.is_empty() {
                api::err_writeln("ShikakeReload requires a plugin name");
                return Ok(());
            }

            // Check if the plugin is tracked.
            {
                let state = state::global();
                let s = state.lock().expect("state lock poisoned");
                if s.get(&name).is_none() {
                    api::err_writeln(&format!("shikake: unknown plugin '{name}'"));
                    return Ok(());
                }
            }

            // Unload.
            if let Err(e) = loader::unload(&name) {
                api::err_writeln(&format!("shikake: unload failed: {e}"));
                return Ok(());
            }

            // Reload via require() — the cpath should already be set from
            // the initial load.
            let cmd = format!("lua require(\"{name}\")");
            let (result, duration) =
                state::timed(|| api::command(&cmd));
            let state = state::global();
            let mut s = state.lock().expect("state lock poisoned");
            match result {
                Ok(()) => {
                    s.mark_loaded(&name, duration);
                    drop(s);
                    let opts = EchoOpts::builder().build();
                    let msg =
                        format!("shikake: reloaded '{name}' ({duration:.2?})");
                    let _ = api::echo(
                        [(msg.as_str(), None::<&str>)],
                        true,
                        &opts,
                    );
                }
                Err(e) => {
                    s.mark_errored(&name, e.to_string());
                    drop(s);
                    api::err_writeln(&format!(
                        "shikake: reload failed for '{name}': {e}"
                    ));
                }
            }

            Ok(())
        })
        .map_err(tane_err)?;

    // Suppress unused variable warning for echo_opts built at top level.
    let _ = echo_opts;

    Ok(())
}
