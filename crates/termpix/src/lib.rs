//! Pixel-level terminal UI plumbing.
//!
//! See `doc/architecture.md` at the repository root for the design.

pub mod compositor;
pub mod graphics;
pub mod input;
pub mod mock;
pub mod output;
pub mod placeholder;
pub mod probe;
pub mod tty;

pub use termpix_core::{geom, raster, scroll};
