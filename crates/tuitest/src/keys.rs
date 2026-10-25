//! Key notation for tests: `a`, `A`, `%`, `Space`, `Enter`, `Tab`, `Esc`,
//! `BS`, `Up`, `F5`, ..., with modifier prefixes `C-` (Ctrl), `M-` (Alt),
//! `S-` (Shift) and `D-` (Super), e.g. `C-h`, `M-x`, `C-S-a`.
//!
//! Keys become libghostty key events the way Ghostty's own frontend makes
//! them (a US layout: the physical key, the text it types, the unshifted
//! character), independently of the code under test (`temzdf-term` has its
//! own translation), so a bug there cannot hide itself.

use libghostty_vt::key::{Action, Event, Key, Mods};

/// One key press.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Press {
    pub key: Key,
    pub mods: Mods,
    /// What the key types (none with Ctrl, Alt or Super, as in Ghostty).
    pub text: Option<String>,
    /// What the key types without modifiers.
    pub unshifted: Option<char>,
}

impl Press {
    pub fn event(&self) -> Event<'static> {
        let mut e = Event::new().expect("key event");
        e.set_key(self.key).set_mods(self.mods).set_action(Action::Press);
        if let Some(c) = self.unshifted {
            e.set_unshifted_codepoint(c);
        }
        if let Some(t) = &self.text {
            e.set_utf8(Some(t.clone()));
            e.set_consumed_mods(self.mods & Mods::SHIFT);
        }
        e
    }
}

pub fn parse(spec: &str) -> Result<Press, String> {
    let mut mods = Mods::empty();
    let mut rest = spec;
    while rest.len() > 2 && rest.as_bytes()[1] == b'-' {
        mods |= match &rest[..2] {
            "C-" => Mods::CTRL,
            "M-" => Mods::ALT,
            "S-" => Mods::SHIFT,
            "D-" => Mods::SUPER,
            _ => break,
        };
        rest = &rest[2..];
    }
    let named = match rest {
        "Space" => Some(Key::Space),
        "Enter" => Some(Key::Enter),
        "Tab" => Some(Key::Tab),
        "Esc" => Some(Key::Escape),
        "BS" => Some(Key::Backspace),
        "Up" => Some(Key::ArrowUp),
        "Down" => Some(Key::ArrowDown),
        "Left" => Some(Key::ArrowLeft),
        "Right" => Some(Key::ArrowRight),
        "Home" => Some(Key::Home),
        "End" => Some(Key::End),
        "PageUp" => Some(Key::PageUp),
        "PageDown" => Some(Key::PageDown),
        "Delete" => Some(Key::Delete),
        "Insert" => Some(Key::Insert),
        f if f.len() > 1 && f.starts_with('F') && f[1..].parse::<usize>().is_ok() => {
            let n: usize = f[1..].parse().unwrap();
            Some(*FUNCTION.get(n.wrapping_sub(1)).ok_or_else(|| format!("no key {f}"))?)
        }
        _ => None,
    };
    if let Some(key) = named {
        let (text, unshifted) = if key == Key::Space { (Some(" ".to_owned()), Some(' ')) } else { (None, None) };
        let text = text.filter(|_| !mods.intersects(Mods::CTRL | Mods::ALT | Mods::SUPER));
        return Ok(Press { key, mods, text, unshifted });
    }
    let mut chars = rest.chars();
    let (Some(c), None) = (chars.next(), chars.next()) else { return Err(format!("unknown key {spec:?}")) };
    let (key, base, shifted) = us_key(c).ok_or_else(|| format!("no US key types {c:?}"))?;
    if shifted {
        mods |= Mods::SHIFT;
    }
    let text = if mods.intersects(Mods::CTRL | Mods::ALT | Mods::SUPER) { None } else { Some(c.to_string()) };
    Ok(Press { key, mods, text, unshifted: Some(base) })
}

const FUNCTION: [Key; 12] =
    [Key::F1, Key::F2, Key::F3, Key::F4, Key::F5, Key::F6, Key::F7, Key::F8, Key::F9, Key::F10, Key::F11, Key::F12];

/// The US-layout key typing `c`, its unshifted character, and whether it
/// needs Shift.
fn us_key(c: char) -> Option<(Key, char, bool)> {
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
    if c.is_ascii_alphabetic() {
        let lower = c.to_ascii_lowercase();
        return Some((LETTERS[(lower as u8 - b'a') as usize], lower, c.is_ascii_uppercase()));
    }
    const OTHERS: [(Key, char, char); 21] = [
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
        (Key::Backquote, '`', '~'),
        (Key::Minus, '-', '_'),
        (Key::Equal, '=', '+'),
        (Key::BracketLeft, '[', '{'),
        (Key::BracketRight, ']', '}'),
        (Key::Backslash, '\\', '|'),
        (Key::Semicolon, ';', ':'),
        (Key::Quote, '\'', '"'),
        (Key::Comma, ',', '<'),
        (Key::Period, '.', '>'),
        (Key::Slash, '/', '?'),
    ];
    OTHERS.iter().find(|(_, b, s)| c == *b || c == *s).map(|(k, b, s)| (*k, *b, c == *s && b != s))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notation() {
        let p = parse("C-h").unwrap();
        assert_eq!((p.key, p.mods, p.text, p.unshifted), (Key::H, Mods::CTRL, None, Some('h')));
        let p = parse("%").unwrap();
        assert_eq!((p.key, p.mods, p.text.as_deref(), p.unshifted), (Key::Digit5, Mods::SHIFT, Some("%"), Some('5')));
        assert_eq!(parse("C-S-a").unwrap().mods, Mods::CTRL | Mods::SHIFT);
        assert_eq!(parse("A").unwrap().mods, Mods::SHIFT);
        assert_eq!(parse("F5").unwrap().key, Key::F5);
        assert_eq!(parse("-").unwrap().key, Key::Minus);
        assert!(parse("Bogus").is_err() && parse("数").is_err());
    }
}
