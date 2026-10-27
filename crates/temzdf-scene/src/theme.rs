//! The default colours, where the session's options set none: what GUI
//! clients draw and what terminal panes tell programs that ask (OSC 4, 10,
//! 11). One definition, so the two agree.

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

/// The 256 colours with `given` (`o.palette`) over xterm's.
pub fn palette(given: &[[u8; 3]]) -> Vec<[u8; 3]> {
    (0..=255u8).map(|i| given.get(i as usize).copied().unwrap_or_else(|| xterm(i))).collect()
}
