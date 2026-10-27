//! Colours the scene leaves to the frontend (`Color::Default`, palette
//! indices): a terminal client's terminal decides them, a GUI decides here,
//! from the session's [`Appearance`] where it gives them, else xterm's
//! palette and our default foreground and background.

use temzdf_scene::{Appearance, Color};

/// The default foreground and background and xterm's palette: the same the
/// session gives terminal panes, so programs asking get what is drawn.
pub use temzdf_scene::theme::{BACKGROUND, FOREGROUND, xterm};

/// The colours in use: the session's appearance over the defaults.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Colors {
    pub foreground: [u8; 3],
    pub background: [u8; 3],
    palette: Vec<[u8; 3]>,
}

impl Default for Colors {
    fn default() -> Self {
        Self::new(&Appearance::default())
    }
}

impl Colors {
    pub fn new(a: &Appearance) -> Self {
        Self {
            foreground: a.foreground.unwrap_or(FOREGROUND),
            background: a.background.unwrap_or(BACKGROUND),
            palette: a.palette.clone(),
        }
    }

    pub fn indexed(&self, i: u8) -> [u8; 3] {
        self.palette.get(i as usize).copied().unwrap_or_else(|| xterm(i))
    }

    /// A text colour; `Default` is `default` (the foreground or the
    /// background, by where it is used).
    pub fn resolve(&self, c: Color, default: [u8; 3]) -> [u8; 3] {
        match c {
            Color::Default => default,
            Color::Indexed(i) => self.indexed(i),
            Color::Rgb(rgb) => rgb,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xterms_palette_and_the_sessions_overrides() {
        assert_eq!(xterm(1), [0xcd, 0, 0]);
        assert_eq!(xterm(16), [0, 0, 0]);
        assert_eq!(xterm(196), [0xff, 0, 0], "the cube's red");
        assert_eq!(xterm(231), [0xff, 0xff, 0xff]);
        assert_eq!(xterm(232), [8, 8, 8]);
        assert_eq!(xterm(255), [0xee, 0xee, 0xee]);
        let c =
            Colors::new(&Appearance { foreground: Some([1, 2, 3]), palette: vec![[9, 9, 9]], ..Appearance::default() });
        assert_eq!(c.resolve(Color::Default, c.foreground), [1, 2, 3]);
        assert_eq!(c.background, BACKGROUND, "not given: ours");
        assert_eq!(c.resolve(Color::Indexed(0), c.foreground), [9, 9, 9], "overridden");
        assert_eq!(c.resolve(Color::Indexed(1), c.foreground), [0xcd, 0, 0], "the rest stay");
        assert_eq!(c.resolve(Color::Rgb([4, 5, 6]), c.foreground), [4, 5, 6]);
    }
}
