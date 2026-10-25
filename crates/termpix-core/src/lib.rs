//! Primitives that do not depend on a terminal: geometry, RGBA raster helpers,
//! kinetic scrolling and key/mouse input events. `termpix` re-exports them; a
//! non-terminal frontend (e.g. a GUI) can use them without the terminal code.

pub mod geom;
pub mod input;
pub mod raster;
pub mod scroll;
