//! Shikake (仕掛け) — Rust-native Neovim plugin loader and lifecycle manager
//!
//! Part of the blnvim-ng distribution — a Rust-native Neovim plugin suite.
//! Built with [`nvim-oxi`](https://github.com/noib3/nvim-oxi) for zero-cost
//! Neovim API bindings.

use nvim_oxi as oxi;

#[oxi::plugin]
fn shikake() -> oxi::Result<()> {
    Ok(())
}
