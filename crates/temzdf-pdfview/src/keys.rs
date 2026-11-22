//! The page view's commands, which scripts run (`action.pdf.*`); the keys
//! that run them are mapped by the scripts (`doc/config.md`).

/// What a key can do in the page view.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Command {
    /// Clear the selection, else the search, else close the pane.
    Cancel,
    ScrollDown,
    ScrollUp,
    ScrollLeft,
    ScrollRight,
    PageDown,
    PageUp,
    HalfPageDown,
    HalfPageUp,
    /// The first page, or page N with a count.
    FirstPage,
    /// The end of the document, or page N with a count.
    LastPage,
    Back,
    Forward,
    ZoomIn,
    ZoomOut,
    FitWidth,
    NextMatch,
    PrevMatch,
    Copy,
    ToggleSidebar,
}

impl Command {
    /// Every command, with its name in commands and scripts (`scroll-down`).
    pub const NAMES: &[(Command, &str)] = &[
        (Command::Cancel, "cancel"),
        (Command::ScrollDown, "scroll-down"),
        (Command::ScrollUp, "scroll-up"),
        (Command::ScrollLeft, "scroll-left"),
        (Command::ScrollRight, "scroll-right"),
        (Command::PageDown, "page-down"),
        (Command::PageUp, "page-up"),
        (Command::HalfPageDown, "half-page-down"),
        (Command::HalfPageUp, "half-page-up"),
        (Command::FirstPage, "first-page"),
        (Command::LastPage, "last-page"),
        (Command::Back, "back"),
        (Command::Forward, "forward"),
        (Command::ZoomIn, "zoom-in"),
        (Command::ZoomOut, "zoom-out"),
        (Command::FitWidth, "fit-width"),
        (Command::NextMatch, "next-match"),
        (Command::PrevMatch, "prev-match"),
        (Command::Copy, "copy"),
        (Command::ToggleSidebar, "toggle-sidebar"),
    ];

    pub fn name(self) -> &'static str {
        Self::NAMES.iter().find(|(c, _)| *c == self).map_or("?", |(_, n)| n)
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::NAMES.iter().find(|(_, n)| *n == name).map(|(c, _)| *c)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_command_has_a_name() {
        for &(c, n) in Command::NAMES {
            assert_eq!(Command::from_name(n), Some(c));
            assert_eq!(c.name(), n);
        }
    }
}
