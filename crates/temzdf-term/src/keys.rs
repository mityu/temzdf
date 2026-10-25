//! Our key events (`termpix-core`, as the outer terminal reported them) as
//! libghostty key events, which its encoder turns into what the program in
//! the pane expects (legacy sequences or the kitty keyboard protocol,
//! depending on the program's modes).

use libghostty_vt::key::{Action, Key, Mods};
use termpix_core::input::{KeyCode, KeyEvent, KeyEventKind, Modifiers};

/// The libghostty key, modifiers, action and text for `k`; `None` for keys
/// that have no physical-key equivalent.
pub fn translate(k: &KeyEvent) -> Option<(Key, Mods, Action, Option<String>)> {
    let action = match k.kind {
        KeyEventKind::Press => Action::Press,
        KeyEventKind::Repeat => Action::Repeat,
        KeyEventKind::Release => Action::Release,
    };
    let mut mods = Mods::empty();
    for (ours, theirs) in [
        (Modifiers::SHIFT, Mods::SHIFT),
        (Modifiers::ALT, Mods::ALT),
        (Modifiers::CTRL, Mods::CTRL),
        (Modifiers::SUPER, Mods::SUPER),
    ] {
        if k.modifiers.contains(ours) {
            mods |= theirs;
        }
    }
    let (key, text) = match k.code {
        KeyCode::Char(c) => {
            let key = char_key(c);
            // The text the key produces, never a control character (the
            // encoder derives those from the key and modifiers).
            let text = k.text.clone().or_else(|| Some(c.to_string())).filter(|t| !t.chars().any(char::is_control));
            if key == Key::Unidentified && text.is_none() {
                return None;
            }
            (key, text)
        }
        KeyCode::Enter => (Key::Enter, None),
        KeyCode::Tab => (Key::Tab, None),
        KeyCode::Backspace => (Key::Backspace, None),
        KeyCode::Escape => (Key::Escape, None),
        KeyCode::Up => (Key::ArrowUp, None),
        KeyCode::Down => (Key::ArrowDown, None),
        KeyCode::Left => (Key::ArrowLeft, None),
        KeyCode::Right => (Key::ArrowRight, None),
        KeyCode::Home => (Key::Home, None),
        KeyCode::End => (Key::End, None),
        KeyCode::PageUp => (Key::PageUp, None),
        KeyCode::PageDown => (Key::PageDown, None),
        KeyCode::Insert => (Key::Insert, None),
        KeyCode::Delete => (Key::Delete, None),
        KeyCode::Begin => (Key::NumpadBegin, None),
        KeyCode::F(n) => (function_key(n)?, None),
        KeyCode::CapsLock => (Key::CapsLock, None),
        KeyCode::ScrollLock => (Key::ScrollLock, None),
        KeyCode::NumLock => (Key::NumLock, None),
        KeyCode::PrintScreen => (Key::PrintScreen, None),
        KeyCode::Pause => (Key::Pause, None),
        KeyCode::Menu => (Key::ContextMenu, None),
        KeyCode::Modifier(_) | KeyCode::Other(_) => return None,
    };
    Some((key, mods, action, text))
}

/// The physical key (US layout) that types `c`.
fn char_key(c: char) -> Key {
    let lower = c.to_ascii_lowercase();
    if lower.is_ascii_lowercase() {
        return LETTERS[(lower as u8 - b'a') as usize];
    }
    match c {
        '0' | ')' => Key::Digit0,
        '1' | '!' => Key::Digit1,
        '2' | '@' => Key::Digit2,
        '3' | '#' => Key::Digit3,
        '4' | '$' => Key::Digit4,
        '5' | '%' => Key::Digit5,
        '6' | '^' => Key::Digit6,
        '7' | '&' => Key::Digit7,
        '8' | '*' => Key::Digit8,
        '9' | '(' => Key::Digit9,
        ' ' => Key::Space,
        '`' | '~' => Key::Backquote,
        '\\' | '|' => Key::Backslash,
        '[' | '{' => Key::BracketLeft,
        ']' | '}' => Key::BracketRight,
        ',' | '<' => Key::Comma,
        '.' | '>' => Key::Period,
        '/' | '?' => Key::Slash,
        ';' | ':' => Key::Semicolon,
        '\'' | '"' => Key::Quote,
        '-' | '_' => Key::Minus,
        '=' | '+' => Key::Equal,
        _ => Key::Unidentified,
    }
}

const LETTERS: [Key; 26] = [
    Key::A,
    Key::B,
    Key::C,
    Key::D,
    Key::E,
    Key::F,
    Key::G,
    Key::H,
    Key::I,
    Key::J,
    Key::K,
    Key::L,
    Key::M,
    Key::N,
    Key::O,
    Key::P,
    Key::Q,
    Key::R,
    Key::S,
    Key::T,
    Key::U,
    Key::V,
    Key::W,
    Key::X,
    Key::Y,
    Key::Z,
];

fn function_key(n: u8) -> Option<Key> {
    const F: [Key; 25] = [
        Key::F1,
        Key::F2,
        Key::F3,
        Key::F4,
        Key::F5,
        Key::F6,
        Key::F7,
        Key::F8,
        Key::F9,
        Key::F10,
        Key::F11,
        Key::F12,
        Key::F13,
        Key::F14,
        Key::F15,
        Key::F16,
        Key::F17,
        Key::F18,
        Key::F19,
        Key::F20,
        Key::F21,
        Key::F22,
        Key::F23,
        Key::F24,
        Key::F25,
    ];
    F.get((n as usize).checked_sub(1)?).copied()
}
