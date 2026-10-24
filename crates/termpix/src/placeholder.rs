//! Unicode placeholders for the kitty graphics protocol (`U=1`).
//!
//! An image transmitted with `U=1` gets a *virtual* placement: it shows wherever
//! the text contains the placeholder character U+10EEEE with the image id in the
//! foreground colour and the row and column (within the image, in cells) encoded
//! as combining diacritics. Because these are ordinary text cells, multiplexers
//! (tmux) keep, clip and redraw them like any other text. See
//! <https://sw.kovidgoyal.net/kitty/graphics-protocol/#unicode-placeholders>.
//!
//! Also here: wrapping escape sequences for tmux passthrough.

use std::io::Write;

pub const PLACEHOLDER: char = '\u{10EEEE}';

/// Row/column diacritics: index n encodes n. This is kitty's
/// `gen/rowcolumn-diacritics.txt` (combining class 230 characters from Unicode
/// 6.0.0 without decompositions), reproduced as data.
#[rustfmt::skip]
pub const DIACRITICS: [u32; 297] = [
    0x0305, 0x030D, 0x030E, 0x0310, 0x0312, 0x033D, 0x033E, 0x033F, 0x0346, 0x034A,
    0x034B, 0x034C, 0x0350, 0x0351, 0x0352, 0x0357, 0x035B, 0x0363, 0x0364, 0x0365,
    0x0366, 0x0367, 0x0368, 0x0369, 0x036A, 0x036B, 0x036C, 0x036D, 0x036E, 0x036F,
    0x0483, 0x0484, 0x0485, 0x0486, 0x0487, 0x0592, 0x0593, 0x0594, 0x0595, 0x0597,
    0x0598, 0x0599, 0x059C, 0x059D, 0x059E, 0x059F, 0x05A0, 0x05A1, 0x05A8, 0x05A9,
    0x05AB, 0x05AC, 0x05AF, 0x05C4, 0x0610, 0x0611, 0x0612, 0x0613, 0x0614, 0x0615,
    0x0616, 0x0617, 0x0657, 0x0658, 0x0659, 0x065A, 0x065B, 0x065D, 0x065E, 0x06D6,
    0x06D7, 0x06D8, 0x06D9, 0x06DA, 0x06DB, 0x06DC, 0x06DF, 0x06E0, 0x06E1, 0x06E2,
    0x06E4, 0x06E7, 0x06E8, 0x06EB, 0x06EC, 0x0730, 0x0732, 0x0733, 0x0735, 0x0736,
    0x073A, 0x073D, 0x073F, 0x0740, 0x0741, 0x0743, 0x0745, 0x0747, 0x0749, 0x074A,
    0x07EB, 0x07EC, 0x07ED, 0x07EE, 0x07EF, 0x07F0, 0x07F1, 0x07F3, 0x0816, 0x0817,
    0x0818, 0x0819, 0x081B, 0x081C, 0x081D, 0x081E, 0x081F, 0x0820, 0x0821, 0x0822,
    0x0823, 0x0825, 0x0826, 0x0827, 0x0829, 0x082A, 0x082B, 0x082C, 0x082D, 0x0951,
    0x0953, 0x0954, 0x0F82, 0x0F83, 0x0F86, 0x0F87, 0x135D, 0x135E, 0x135F, 0x17DD,
    0x193A, 0x1A17, 0x1A75, 0x1A76, 0x1A77, 0x1A78, 0x1A79, 0x1A7A, 0x1A7B, 0x1A7C,
    0x1B6B, 0x1B6D, 0x1B6E, 0x1B6F, 0x1B70, 0x1B71, 0x1B72, 0x1B73, 0x1CD0, 0x1CD1,
    0x1CD2, 0x1CDA, 0x1CDB, 0x1CE0, 0x1DC0, 0x1DC1, 0x1DC3, 0x1DC4, 0x1DC5, 0x1DC6,
    0x1DC7, 0x1DC8, 0x1DC9, 0x1DCB, 0x1DCC, 0x1DD1, 0x1DD2, 0x1DD3, 0x1DD4, 0x1DD5,
    0x1DD6, 0x1DD7, 0x1DD8, 0x1DD9, 0x1DDA, 0x1DDB, 0x1DDC, 0x1DDD, 0x1DDE, 0x1DDF,
    0x1DE0, 0x1DE1, 0x1DE2, 0x1DE3, 0x1DE4, 0x1DE5, 0x1DE6, 0x1DFE, 0x20D0, 0x20D1,
    0x20D4, 0x20D5, 0x20D6, 0x20D7, 0x20DB, 0x20DC, 0x20E1, 0x20E7, 0x20E9, 0x20F0,
    0x2CEF, 0x2CF0, 0x2CF1, 0x2DE0, 0x2DE1, 0x2DE2, 0x2DE3, 0x2DE4, 0x2DE5, 0x2DE6,
    0x2DE7, 0x2DE8, 0x2DE9, 0x2DEA, 0x2DEB, 0x2DEC, 0x2DED, 0x2DEE, 0x2DEF, 0x2DF0,
    0x2DF1, 0x2DF2, 0x2DF3, 0x2DF4, 0x2DF5, 0x2DF6, 0x2DF7, 0x2DF8, 0x2DF9, 0x2DFA,
    0x2DFB, 0x2DFC, 0x2DFD, 0x2DFE, 0x2DFF, 0xA66F, 0xA67C, 0xA67D, 0xA6F0, 0xA6F1,
    0xA8E0, 0xA8E1, 0xA8E2, 0xA8E3, 0xA8E4, 0xA8E5, 0xA8E6, 0xA8E7, 0xA8E8, 0xA8E9,
    0xA8EA, 0xA8EB, 0xA8EC, 0xA8ED, 0xA8EE, 0xA8EF, 0xA8F0, 0xA8F1, 0xAAB0, 0xAAB2,
    0xAAB3, 0xAAB7, 0xAAB8, 0xAABE, 0xAABF, 0xAAC1, 0xFE20, 0xFE21, 0xFE22, 0xFE23,
    0xFE24, 0xFE25, 0xFE26, 0x10A0F, 0x10A38, 0x1D185, 0x1D186, 0x1D187, 0x1D188, 0x1D189,
    0x1D1AA, 0x1D1AB, 0x1D1AC, 0x1D1AD, 0x1D242, 0x1D243, 0x1D244,
];

/// Largest number of rows or columns of an image that placeholders can address.
pub const MAX_CELLS: u32 = DIACRITICS.len() as u32;

fn mark(n: u32) -> char {
    char::from_u32(DIACRITICS[n as usize]).unwrap_or(' ')
}

/// Write placeholder text showing `rows` x `cols` cells of image `id`, starting
/// at image cell row `image_row`, at screen cell (`row`, `col`) (zero-based).
/// Ids must fit in 24 bits (they are encoded as a true-colour foreground). The
/// foreground colour is reset afterwards.
pub fn write_cells(out: &mut Vec<u8>, id: u32, image_row: u32, row: u32, col: u32, rows: u32, cols: u32) {
    assert!(id <= 0xff_ffff, "placeholder image ids are 24-bit");
    write!(out, "\x1b[38;2;{};{};{}m", (id >> 16) & 0xff, (id >> 8) & 0xff, id & 0xff).unwrap();
    let mut buf = [0u8; 4];
    for y in 0..rows.min(MAX_CELLS.saturating_sub(image_row)) {
        write!(out, "\x1b[{};{}H", row + y + 1, col + 1).unwrap();
        let row_mark = mark(image_row + y);
        for x in 0..cols.min(MAX_CELLS) {
            out.extend_from_slice(PLACEHOLDER.encode_utf8(&mut buf).as_bytes());
            out.extend_from_slice(row_mark.encode_utf8(&mut buf).as_bytes());
            out.extend_from_slice(mark(x).encode_utf8(&mut buf).as_bytes());
        }
    }
    out.extend_from_slice(b"\x1b[39m");
}

/// Wrap one complete escape sequence for tmux passthrough, for sequences tmux
/// would otherwise swallow or interpret itself (e.g. OSC 52 clipboard writes
/// with tmux's default `set-clipboard external`).
pub fn tmux_passthrough(seq: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(seq.len() + 16);
    out.extend_from_slice(b"\x1bPtmux;");
    for &c in seq {
        if c == 0x1b {
            out.push(0x1b);
        }
        out.push(c);
    }
    out.extend_from_slice(b"\x1b\\");
    out
}

/// Wrap every APC sequence (`ESC _ ... ESC \`, i.e. kitty graphics commands) in
/// `bytes` into a tmux passthrough DCS (`ESC P tmux; <ESC doubled> ESC \`), so
/// tmux forwards it to the outer terminal. Needs `set -g allow-passthrough on`.
/// Everything else (cursor moves, text) is left for tmux to interpret.
pub fn wrap_for_tmux(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len() + bytes.len() / 64);
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i..].starts_with(b"\x1b_")
            && let Some(end) = bytes[i + 2..].windows(2).position(|w| w == b"\x1b\\")
        {
            let seq = &bytes[i..i + 2 + end + 2];
            out.extend_from_slice(&tmux_passthrough(seq));
            i += seq.len();
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_matches_the_spec() {
        assert_eq!(&DIACRITICS[..3], &[0x0305, 0x030D, 0x030E]);
        assert_eq!(DIACRITICS[296], 0x1D244);
    }

    #[test]
    fn cells_encode_id_row_and_column() {
        let mut out = Vec::new();
        write_cells(&mut out, 0x01_02_03, 1, 4, 0, 1, 2);
        let s = String::from_utf8(out).unwrap();
        assert!(s.starts_with("\x1b[38;2;1;2;3m\x1b[5;1H"), "{s:?}");
        let tail: Vec<char> = s["\x1b[38;2;1;2;3m\x1b[5;1H".len()..].chars().take(6).collect();
        assert_eq!(tail, ['\u{10EEEE}', '\u{030D}', '\u{0305}', '\u{10EEEE}', '\u{030D}', '\u{030D}']);
        assert!(s.ends_with("\x1b[39m"));
    }

    #[test]
    fn tmux_wrapping_doubles_escapes_and_keeps_other_bytes() {
        let wrapped = wrap_for_tmux(b"\x1b[H\x1b_Ga=p,i=1\x1b\\x");
        assert_eq!(wrapped, b"\x1b[H\x1bPtmux;\x1b\x1b_Ga=p,i=1\x1b\x1b\\\x1b\\x".to_vec());
    }
}
