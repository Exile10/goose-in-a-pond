//! Voice primitives shared by every GIAP surface.
//!
//! A LEAF crate: std only, no `pond-core`, no tokio, no cpal, no anyhow. That
//! shape was originally forced by a second cargo workspace under
//! `pond-desktop/src-tauri`, which could not depend on `pond-core`. That
//! workspace is gone, so the constraint is now deliberate rather than
//! imposed — see `Cargo.toml` for why it is kept.
//!
//! See `docs/` and the module docs for what moved and from where.

pub mod barge;
pub mod control;
pub mod dsp;
pub mod text;
pub mod turn;
