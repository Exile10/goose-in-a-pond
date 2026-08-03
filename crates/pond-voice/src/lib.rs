//! Voice primitives shared by every GIAP surface.
//!
//! A LEAF crate: std only, no `pond-core`, no tokio, no cpal, no anyhow. That
//! constraint is load-bearing — `pond-desktop/src-tauri` declares its own
//! `[workspace]` and cannot depend on `pond-core`, so a leaf is the only shape
//! that lets the desktop shell, the server, and both voice adapters share one
//! implementation instead of four.
//!
//! See `docs/` and the module docs for what moved and from where.

pub mod barge;
pub mod dsp;
pub mod text;
pub mod turn;
