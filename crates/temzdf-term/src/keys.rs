//! Our key events (`termpix-core`, as the outer terminal reported them) as
//! libghostty key events, which its encoder turns into what the program in
//! the pane expects (legacy sequences or the kitty keyboard protocol,
//! depending on the program's modes).

use libghostty_vt::Terminal;
use libghostty_vt::key::{self, Action, Encoder, Key, Mods};
use termpix_core::input::{KeyCode, KeyEvent, KeyEventKind, Modifiers};

/// A key event as libghostty's encoder wants it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Translated {
    pub key: Key,
    pub mods: Mods,
    /// The modifiers that went into producing `text` (Shift for `A` or `%`):
    /// the kitty keyboard protocol reports them as part of the text, not as
    /// modifiers.
    pub consumed: Mods,
    pub action: Action,
    /// What the key types, if anything (never a control character: the
    /// encoder derives those from the key and modifiers).
    pub text: Option<String>,
    /// The character the key types without modifiers (`a` for `A`, `5` for
    /// `%`), which the kitty keyboard protocol reports as the key code.
    pub unshifted: Option<char>,
}

/// Our modifiers as libghostty's.
pub fn mods(m: Modifiers) -> Mods {
    let mut mods = Mods::empty();
    for (ours, theirs) in [
        (Modifiers::SHIFT, Mods::SHIFT),
        (Modifiers::ALT, Mods::ALT),
        (Modifiers::CTRL, Mods::CTRL),
        (Modifiers::SUPER, Mods::SUPER),
    ] {
        if m.contains(ours) {
            mods |= theirs;
        }
    }
    mods
}

/// `k` for libghostty's encoder; `None` for keys that have no physical-key
/// equivalent.
pub fn translate(k: &KeyEvent) -> Option<Translated> {
    let action = match k.kind {
        KeyEventKind::Press => Action::Press,
        KeyEventKind::Repeat => Action::Repeat,
        KeyEventKind::Release => Action::Release,
    };
    let mods = mods(k.modifiers);
    let mut text = None;
    let mut unshifted = None;
    let key = match k.code {
        // Control characters reported as code points.
        KeyCode::Char('\r' | '\n') => Key::Enter,
        KeyCode::Char('\t') => Key::Tab,
        KeyCode::Char('\x08' | '\x7f') => Key::Backspace,
        KeyCode::Char('\x1b') => Key::Escape,
        KeyCode::Char(c) => {
            let (key, base) = char_key(c);
            unshifted = base;
            // Ctrl without Shift types nothing: with text, the encoder would
            // send the bare character (C-h as `h`) when the program uses the
            // kitty keyboard protocol.
            // A character of no US key (another layout) has no key to
            // carry Ctrl: it goes as its text then, rather than not at all.
            if !(mods.contains(Mods::CTRL) && !mods.contains(Mods::SHIFT)) || key == Key::Unidentified {
                text = k.text.clone().or_else(|| Some(c.to_string())).filter(|t| !t.chars().any(char::is_control));
            }
            if key == Key::Unidentified && text.is_none() {
                return None;
            }
            key
        }
        KeyCode::Enter => Key::Enter,
        KeyCode::Tab => Key::Tab,
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Escape => Key::Escape,
        KeyCode::Up => Key::ArrowUp,
        KeyCode::Down => Key::ArrowDown,
        KeyCode::Left => Key::ArrowLeft,
        KeyCode::Right => Key::ArrowRight,
        KeyCode::Home => Key::Home,
        KeyCode::End => Key::End,
        KeyCode::PageUp => Key::PageUp,
        KeyCode::PageDown => Key::PageDown,
        KeyCode::Insert => Key::Insert,
        KeyCode::Delete => Key::Delete,
        KeyCode::Begin => Key::NumpadBegin,
        KeyCode::F(n) => function_key(n)?,
        KeyCode::CapsLock => Key::CapsLock,
        KeyCode::ScrollLock => Key::ScrollLock,
        KeyCode::NumLock => Key::NumLock,
        KeyCode::PrintScreen => Key::PrintScreen,
        KeyCode::Pause => Key::Pause,
        KeyCode::Menu => Key::ContextMenu,
        KeyCode::Modifier(_) | KeyCode::Other(_) => return None,
    };
    let consumed = if text.is_some() { mods & Mods::SHIFT } else { Mods::empty() };
    Some(Translated { key, mods, consumed, action, text, unshifted })
}

/// The bytes the program in `terminal` expects for `k` (in the keyboard
/// modes it asked for). Empty for keys it has no encoding for.
pub fn encode(encoder: &mut Encoder<'_>, terminal: &Terminal<'_, '_>, k: &KeyEvent) -> Vec<u8> {
    let mut out = Vec::new();
    let Some(t) = translate(k) else { return out };
    let Ok(mut event) = key::Event::new() else { return out };
    event.set_key(t.key).set_mods(t.mods).set_consumed_mods(t.consumed).set_action(t.action);
    if let Some(c) = t.unshifted {
        event.set_unshifted_codepoint(c);
    }
    if let Some(text) = t.text {
        event.set_utf8(Some(text));
    }
    // Alt in our events is Alt: the outer terminal or the GUI already decided
    // whether Option composes characters. On macOS libghostty would treat it
    // as composing (Alt+x as `x`) unless told, and the call before resets it.
    encoder.set_options_from_terminal(terminal).set_macos_option_as_alt(key::OptionAsAlt::True);
    if encoder.encode_to_vec(&event, &mut out).is_err() {
        out.clear();
    }
    out
}

/// The physical key (US layout) that types `c`, and what it types without
/// Shift.
fn char_key(c: char) -> (Key, Option<char>) {
    let lower = c.to_ascii_lowercase();
    if lower.is_ascii_lowercase() {
        return (LETTERS[(lower as u8 - b'a') as usize], Some(lower));
    }
    const KEYS: [(Key, char, char); 21] = [
        (Key::Digit0, '0', ')'),
        (Key::Digit1, '1', '!'),
        (Key::Digit2, '2', '@'),
        (Key::Digit3, '3', '#'),
        (Key::Digit4, '4', '$'),
        (Key::Digit5, '5', '%'),
        (Key::Digit6, '6', '^'),
        (Key::Digit7, '7', '&'),
        (Key::Digit8, '8', '*'),
        (Key::Digit9, '9', '('),
        (Key::Space, ' ', ' '),
        (Key::Backquote, '`', '~'),
        (Key::Backslash, '\\', '|'),
        (Key::BracketLeft, '[', '{'),
        (Key::BracketRight, ']', '}'),
        (Key::Comma, ',', '<'),
        (Key::Period, '.', '>'),
        (Key::Slash, '/', '?'),
        (Key::Semicolon, ';', ':'),
        (Key::Quote, '\'', '"'),
        (Key::Minus, '-', '_'),
    ];
    match KEYS.iter().find(|(_, base, shifted)| c == *base || c == *shifted) {
        Some((key, base, _)) => (*key, Some(*base)),
        None if c == '=' || c == '+' => (Key::Equal, Some('=')),
        // Not on a US keyboard: the character itself is the key code.
        None => (Key::Unidentified, Some(c)),
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

#[cfg(test)]
mod tests {
    use super::*;
    use libghostty_vt::TerminalOptions;

    /// What a program in a terminal with these keyboard modes receives.
    fn sent(modes: &[u8], code: KeyCode, m: Modifiers) -> String {
        let mut terminal = Terminal::new(TerminalOptions { cols: 10, rows: 3, max_scrollback: 0 }).unwrap();
        terminal.vt_write(modes);
        let mut encoder = Encoder::new().unwrap();
        let mut k = KeyEvent::new(code, m);
        if let KeyCode::Char(c) = code
            && !m.contains(Modifiers::CTRL)
        {
            k.text = Some(c.to_string());
        }
        String::from_utf8(encode(&mut encoder, &terminal, &k)).unwrap()
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot call libghostty-vt")]
    fn legacy_encoding() {
        let s = |c, m| sent(b"", c, m);
        assert_eq!(s(KeyCode::Char('h'), Modifiers::CTRL), "\x08");
        assert_eq!(s(KeyCode::Char('c'), Modifiers::CTRL), "\x03");
        assert_eq!(s(KeyCode::Char('x'), Modifiers::ALT), "\x1bx");
        assert_eq!(s(KeyCode::Char('A'), Modifiers::SHIFT), "A");
        assert_eq!(s(KeyCode::Char('%'), Modifiers::SHIFT), "%");
        assert_eq!(s(KeyCode::Char('é'), Modifiers::NONE), "é");
        assert_eq!(s(KeyCode::Up, Modifiers::NONE), "\x1b[A");
        assert_eq!(s(KeyCode::Char('\n'), Modifiers::NONE), "\r");
        // Ctrl with a character of another layout: not dropped.
        assert_eq!(s(KeyCode::Char('ф'), Modifiers::CTRL), "\x1b[1092;5u");
    }

    /// Programs that turn on the kitty keyboard protocol (fish 4, neovim)
    /// get Ctrl combinations as `CSI code;mods u` and typed text as text.
    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot call libghostty-vt")]
    fn kitty_keyboard_encoding() {
        let s = |c, m| sent(b"\x1b[>1u", c, m);
        assert_eq!(s(KeyCode::Char('h'), Modifiers::CTRL), "\x1b[104;5u");
        assert_eq!(s(KeyCode::Char('a'), Modifiers::CTRL | Modifiers::SHIFT), "\x1b[97;6u");
        assert_eq!(s(KeyCode::Char('x'), Modifiers::ALT), "\x1b[120;3u");
        assert_eq!(s(KeyCode::Char('A'), Modifiers::SHIFT), "A");
        assert_eq!(s(KeyCode::Char('%'), Modifiers::SHIFT), "%");
        assert_eq!(s(KeyCode::Char('a'), Modifiers::NONE), "a");
        assert_eq!(s(KeyCode::Escape, Modifiers::NONE), "\x1b[27u");
        assert_eq!(s(KeyCode::Char('ф'), Modifiers::CTRL), "\x1b[1092;5u");
    }
}
