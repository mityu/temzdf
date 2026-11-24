//! Controlling terminal: raw mode, terminal modes, reading input, and reliable
//! restoration (on drop, on panic).
//!
//! Unix only so far (`unix`): on Windows [`Tty::open`] fails with
//! [`std::io::ErrorKind::Unsupported`] until the console is implemented
//! (`windows`). What is shared lives here.

use crate::geom::{CellSize, Geometry};
use crate::input::Event;

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub(crate) use unix::wait_fd;
#[cfg(unix)]
pub use unix::{StdioRedirect, Tty, TtyReader, spawn_input};
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::{StdioRedirect, Tty, TtyReader, spawn_input};

/// Which terminal modes to enable. Defaults enable everything termpix uses.
#[derive(Clone, Copy, Debug)]
pub struct Modes {
    pub alternate_screen: bool,
    /// Any-motion mouse tracking (1003) with SGR encoding (1006).
    pub mouse: bool,
    /// SGR-pixel mouse coordinates (1016). Only enable when the terminal supports it.
    pub pixel_mouse: bool,
    /// Kitty keyboard protocol flags to push (`CSI > flags u`); 0 to skip.
    pub keyboard_flags: u32,
    pub bracketed_paste: bool,
    pub focus_events: bool,
    /// In-band resize reports (2048).
    pub resize_reports: bool,
}

impl Default for Modes {
    fn default() -> Self {
        Self {
            alternate_screen: true,
            mouse: true,
            pixel_mouse: false,
            keyboard_flags: 31,
            bracketed_paste: true,
            focus_events: true,
            resize_reports: true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Winsize {
    pub rows: u16,
    pub cols: u16,
    pub width_px: u16,
    pub height_px: u16,
}

impl Winsize {
    /// Geometry derived from the kernel's pixel size, if it reports one.
    pub fn geometry(&self) -> Option<Geometry> {
        if self.rows == 0 || self.cols == 0 || self.width_px == 0 || self.height_px == 0 {
            return None;
        }
        Some(Geometry {
            cols: self.cols,
            rows: self.rows,
            cell: CellSize {
                width: self.width_px as u32 / self.cols as u32,
                height: self.height_px as u32 / self.rows as u32,
            },
        })
    }

    /// [`Winsize::geometry`], or where the kernel reports no pixel size
    /// (mosh, WezTerm on WSL, some multiplexers) the new grid with the cell
    /// size known so far.
    pub fn geometry_or(&self, cell: CellSize) -> Option<Geometry> {
        if self.rows == 0 || self.cols == 0 {
            return None;
        }
        Some(self.geometry().unwrap_or(Geometry { cols: self.cols, rows: self.rows, cell }))
    }
}

/// Things the input thread reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TermEvent {
    Input(Event),
    /// SIGWINCH: re-read the window size.
    WindowChanged,
    /// SIGTERM / SIGHUP, or the input thread ended (the terminal went, or
    /// a bug): the application should exit.
    Terminate,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resizes_without_pixel_sizes_keep_the_cell_size() {
        let cell = CellSize { width: 9, height: 18 };
        let with_px = Winsize { rows: 30, cols: 100, width_px: 1000, height_px: 600 };
        assert_eq!(with_px.geometry_or(cell).unwrap().cell, CellSize { width: 10, height: 20 });
        let without = Winsize { rows: 40, cols: 120, width_px: 0, height_px: 0 };
        assert_eq!(without.geometry(), None);
        assert_eq!(without.geometry_or(cell), Some(Geometry { cols: 120, rows: 40, cell }));
        assert_eq!(Winsize { rows: 0, cols: 0, width_px: 0, height_px: 0 }.geometry_or(cell), None);
        let report = crate::input::ResizeReport { rows: 40, cols: 120, height_px: 800, width_px: 1200 };
        assert_eq!(report.geometry(), Some(Geometry { cols: 120, rows: 40, cell: CellSize { width: 10, height: 20 } }));
    }
}
