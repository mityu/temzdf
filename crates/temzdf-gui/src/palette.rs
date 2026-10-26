//! Colours the scene leaves to the frontend (`Color::Default`, palette
//! indices): a terminal client's terminal decides them, a GUI decides here,
//! from the session's [`Appearance`] where it gives them, else xterm's
//! palette and our default foreground and background.

use temzdf_scene::{Appearance, Color};

/// The default foreground and background when the session sets none.
pub const FOREGROUND: [u8; 3] = [0xd8, 0xd8, 0xd8];
pub const BACKGROUND: [u8; 3] = [0x1e, 0x1e, 0x1e];

/// xterm's 16 base colours.
const BASE: [[u8; 3]; 16] = [
    [0x00, 0x00, 0x00],
    [0xcd, 0x00, 0x00],
    [0x00, 0xcd, 0x00],
    [0xcd, 0xcd, 0x00],
    [0x00, 0x00, 0xee],
    [0xcd, 0x00, 0xcd],
    [0x00, 0xcd, 0xcd],
    [0xe5, 0xe5, 0xe5],
    [0x7f, 0x7f, 0x7f],
    [0xff, 0x00, 0x00],
    [0x00, 0xff, 0x00],
    [0xff, 0xff, 0x00],
    [0x5c, 0x5c, 0xff],
    [0xff, 0x00, 0xff],
    [0x00, 0xff, 0xff],
    [0xff, 0xff, 0xff],
];

/// xterm's colour `i` of 256: the base 16, a 6x6x6 cube, 24 greys.
pub fn xterm(i: u8) -> [u8; 3] {
    match i {
        0..=15 => BASE[i as usize],
        16..=231 => {
            let i = i - 16;
            let level = |v: u8| if v == 0 { 0 } else { 55 + 40 * v };
            [level(i / 36), level(i / 6 % 6), level(i % 6)]
        }
        _ => {
            let v = 8 + 10 * (i - 232);
            [v, v, v]
        }
    }
}

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
