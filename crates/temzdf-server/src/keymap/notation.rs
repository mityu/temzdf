//! Keys in Neovim's notation (`j`, `G`, `<C-h>`, `<S-Tab>`, `<CR>`, `<lt>`)
//! and the normal form both mappings and typed keys are compared in.
//!
//! Normal form: printable characters carry Shift in the character (`%`, `A`),
//! except for letters typed with Shift, which are `<S-a>`; with Ctrl, Alt or
//! Super a letter is lower case and Shift is a modifier (`<M-A>` is
//! `<M-S-a>`), except that `<C-A>` is `<C-a>`, as in Neovim. Lock keys never
//! count.
//!
//! Keys the kitty keyboard protocol tells apart but legacy terminals do not
//! have a fallback ([`Stroke::fallback`]): `<S-a>` falls back to `A`,
//! `<C-i>` to `<Tab>`, `<C-m>` to `<CR>`, `<C-[>` to `<Esc>`, `<C-h>` to
//! `<BS>`. A mapping can use either; a typed key matches its own form first,
//! then the fallback (see `modes::Matcher`), so `<C-i>` can be mapped apart
//! from `<Tab>` where the terminal reports it, and does what `<Tab>` does
//! otherwise.

use anyhow::{Result, bail};
use termpix_core::input::{KeyCode, KeyEvent, Modifiers};

/// One key with modifiers, in normal form.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Stroke {
    pub code: KeyCode,
    pub mods: Modifiers,
}

const KEPT: [Modifiers; 4] = [Modifiers::SHIFT, Modifiers::ALT, Modifiers::CTRL, Modifiers::SUPER];

fn kept(m: Modifiers) -> Modifiers {
    KEPT.iter().filter(|k| m.contains(**k)).fold(Modifiers::NONE, |a, k| a | *k)
}

fn has(m: Modifiers, k: Modifiers) -> bool {
    m.contains(k)
}

fn without(m: Modifiers, k: Modifiers) -> Modifiers {
    Modifiers::from_bits(m.bits() & !k.bits())
}

impl Stroke {
    /// The normal form of a typed key.
    pub fn of(ev: &KeyEvent) -> Self {
        let mods = kept(ev.modifiers);
        Self::normal(ev.code, mods, false)
    }

    /// `from_notation`: the key came from notation, where `<C-A>` means
    /// `<C-a>`; a typed `A` with Ctrl was typed with Shift.
    fn normal(code: KeyCode, mods: Modifiers, from_notation: bool) -> Self {
        let KeyCode::Char(c) = code else { return Self { code, mods } };
        let chord = has(mods, Modifiers::CTRL) || has(mods, Modifiers::ALT) || has(mods, Modifiers::SUPER);
        if !chord {
            // A letter with Shift keeps it (`<S-a>`, falling back to `A`);
            // other characters already carry it (`%`).
            if has(mods, Modifiers::SHIFT) && c.is_ascii_alphabetic() {
                return Self { code: KeyCode::Char(c.to_ascii_lowercase()), mods: Modifiers::SHIFT };
            }
            return Self { code, mods: without(mods, Modifiers::SHIFT) };
        }
        if c.is_ascii_alphabetic() {
            let ctrl_only = has(mods, Modifiers::CTRL) && !has(mods, Modifiers::ALT) && !has(mods, Modifiers::SUPER);
            let shift = has(mods, Modifiers::SHIFT) || (c.is_ascii_uppercase() && !(from_notation && ctrl_only));
            let mods = if shift { mods | Modifiers::SHIFT } else { without(mods, Modifiers::SHIFT) };
            return Self { code: KeyCode::Char(c.to_ascii_lowercase()), mods };
        }
        Self { code, mods: without(mods, Modifiers::SHIFT) }
    }

    /// What this key is on terminals that cannot tell it apart.
    pub fn fallback(&self) -> Option<Stroke> {
        let plain = |code| Some(Stroke { code, mods: Modifiers::NONE });
        match (self.code, self.mods) {
            (KeyCode::Char(c), Modifiers::SHIFT) if c.is_ascii_lowercase() => {
                plain(KeyCode::Char(c.to_ascii_uppercase()))
            }
            (KeyCode::Char('i'), Modifiers::CTRL) => plain(KeyCode::Tab),
            (KeyCode::Char('m'), Modifiers::CTRL) => plain(KeyCode::Enter),
            (KeyCode::Char('['), Modifiers::CTRL) => plain(KeyCode::Escape),
            (KeyCode::Char('h'), Modifiers::CTRL) => plain(KeyCode::Backspace),
            _ => None,
        }
    }

    /// A press of this key, with the text a terminal would report for it.
    pub fn event(self) -> KeyEvent {
        let mut code = self.code;
        let mut mods = self.mods;
        if let KeyCode::Char(c) = code
            && has(mods, Modifiers::SHIFT)
            && c.is_ascii_lowercase()
        {
            code = KeyCode::Char(c.to_ascii_uppercase());
            mods = without(mods, Modifiers::SHIFT) | Modifiers::SHIFT;
        }
        let mut ev = KeyEvent::new(code, mods);
        if let KeyCode::Char(c) = code
            && !has(mods, Modifiers::CTRL)
            && !has(mods, Modifiers::ALT)
            && !has(mods, Modifiers::SUPER)
        {
            ev.text = Some(c.to_string());
        }
        ev
    }

    /// Back to notation (`j`, `<lt>`, `<C-S-a>`, `<CR>`).
    pub fn notation(&self) -> String {
        let mut prefix = String::new();
        for (m, p) in
            [(Modifiers::CTRL, "C-"), (Modifiers::SHIFT, "S-"), (Modifiers::ALT, "M-"), (Modifiers::SUPER, "D-")]
        {
            if has(self.mods, m) {
                prefix.push_str(p);
            }
        }
        let name: String = match self.code {
            KeyCode::Char('<') => "lt".into(),
            KeyCode::Char('\\') => "Bslash".into(),
            KeyCode::Char('|') => "Bar".into(),
            KeyCode::Char(' ') => "Space".into(),
            KeyCode::Char(c) if prefix.is_empty() => return c.to_string(),
            KeyCode::Char(c) => c.to_string(),
            other => match NAMES.iter().find(|(_, k)| *k == other) {
                Some((n, _)) => (*n).into(),
                None => match other {
                    KeyCode::F(n) => format!("F{n}"),
                    KeyCode::Other(n) => format!("K{n}"),
                    _ => "?".into(),
                },
            },
        };
        format!("<{prefix}{name}>")
    }
}

/// Key names, the preferred spelling first.
const NAMES: &[(&str, KeyCode)] = &[
    ("CR", KeyCode::Enter),
    ("Enter", KeyCode::Enter),
    ("Return", KeyCode::Enter),
    ("Esc", KeyCode::Escape),
    ("Tab", KeyCode::Tab),
    ("BS", KeyCode::Backspace),
    ("Backspace", KeyCode::Backspace),
    ("Del", KeyCode::Delete),
    ("Delete", KeyCode::Delete),
    ("Insert", KeyCode::Insert),
    ("Ins", KeyCode::Insert),
    ("Up", KeyCode::Up),
    ("Down", KeyCode::Down),
    ("Left", KeyCode::Left),
    ("Right", KeyCode::Right),
    ("Home", KeyCode::Home),
    ("End", KeyCode::End),
    ("PageUp", KeyCode::PageUp),
    ("PageDown", KeyCode::PageDown),
    ("Space", KeyCode::Char(' ')),
    ("lt", KeyCode::Char('<')),
    ("Bslash", KeyCode::Char('\\')),
    ("Bar", KeyCode::Char('|')),
];

/// Parse a key sequence (`gg`, `<C-b>%`, `/needle<CR>`).
pub fn parse(s: &str) -> Result<Vec<Stroke>> {
    let mut out = Vec::new();
    let mut rest = s;
    while let Some(c) = rest.chars().next() {
        if c == '<'
            && let Some(end) = rest.find('>')
            && let Ok(stroke) = parse_special(&rest[1..end])
        {
            out.push(stroke);
            rest = &rest[end + 1..];
            continue;
        }
        // A `<` that does not start a key name is itself (as in Vim).
        out.push(Stroke::normal(KeyCode::Char(c), Modifiers::NONE, true));
        rest = &rest[c.len_utf8()..];
    }
    if out.is_empty() {
        bail!("empty key sequence");
    }
    Ok(out)
}

/// The inside of `<...>`: modifiers, then a name or one character.
fn parse_special(inner: &str) -> Result<Stroke> {
    let mut mods = Modifiers::NONE;
    let mut rest = inner;
    while rest.len() > 2 && rest.as_bytes()[1] == b'-' {
        mods |= match rest.as_bytes()[0].to_ascii_uppercase() {
            b'C' => Modifiers::CTRL,
            b'S' => Modifiers::SHIFT,
            b'M' | b'A' => Modifiers::ALT,
            b'D' => Modifiers::SUPER,
            _ => break,
        };
        rest = &rest[2..];
    }
    let code = if let Some((_, k)) = NAMES.iter().find(|(n, _)| n.eq_ignore_ascii_case(rest)) {
        *k
    } else if rest.len() > 1
        && rest.as_bytes()[0].eq_ignore_ascii_case(&b'F')
        && let Ok(n) = rest[1..].parse::<u8>()
        && (1..=35).contains(&n)
    {
        KeyCode::F(n)
    } else {
        let mut chars = rest.chars();
        match (chars.next(), chars.next()) {
            (Some(c), None) if mods != Modifiers::NONE => KeyCode::Char(c),
            _ => bail!("unknown key <{inner}>"),
        }
    };
    Ok(Stroke::normal(code, mods, true))
}

/// A whole sequence back to notation.
pub fn notation(keys: &[Stroke]) -> String {
    keys.iter().map(Stroke::notation).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(s: &str) -> Stroke {
        let v = parse(s).unwrap();
        assert_eq!(v.len(), 1, "{s}");
        v[0]
    }

    fn typed(code: KeyCode, mods: Modifiers) -> Stroke {
        Stroke::of(&KeyEvent::new(code, mods))
    }

    #[test]
    fn sequences_and_names() {
        assert_eq!(parse("gg").unwrap().len(), 2);
        let v = parse("<C-b>%/a<CR>").unwrap();
        assert_eq!(notation(&v), "<C-b>%/a<CR>");
        assert_eq!(one("<cr>"), one("<Enter>"));
        assert_eq!(one("<lt>").code, KeyCode::Char('<'));
        assert_eq!(parse("a<b").unwrap().len(), 3, "a lone < is itself");
        assert_eq!(one("<F12>").code, KeyCode::F(12));
        assert_eq!(one("<S-Tab>"), Stroke { code: KeyCode::Tab, mods: Modifiers::SHIFT });
        assert!(parse("<Bogus>x").unwrap().len() > 2, "an unknown name is plain text");
        assert!(parse("").is_err());
    }

    #[test]
    fn shift_is_folded_like_neovim() {
        assert_ne!(one("<S-a>"), one("A"), "told apart where the terminal can");
        assert_eq!(one("<S-a>").fallback(), Some(one("A")));
        assert_eq!(one("<C-i>").fallback(), Some(one("<Tab>")));
        assert_eq!(one("<C-m>").fallback(), Some(one("<CR>")));
        assert_eq!(one("A").fallback(), None);
        assert_eq!(one("<C-A>"), one("<C-a>"), "<C-A> is <C-a>");
        assert_eq!(one("<M-A>"), one("<M-S-a>"), "with Alt, case is Shift");
        assert_ne!(one("<C-S-a>"), one("<C-a>"));
        // Typed keys: the character carries Shift; with Ctrl an upper-case
        // letter was typed with Shift.
        assert_eq!(typed(KeyCode::Char('A'), Modifiers::SHIFT), one("<S-a>"), "Shift+a (legacy terminals report A so)");
        assert_eq!(typed(KeyCode::Char('A'), Modifiers::NONE), one("A"), "A with Caps Lock (kitty protocol)");
        assert_eq!(typed(KeyCode::Char('%'), Modifiers::SHIFT), one("%"));
        assert_eq!(typed(KeyCode::Char('a'), Modifiers::CTRL), one("<C-a>"));
        assert_eq!(typed(KeyCode::Char('A'), Modifiers::CTRL), one("<C-S-a>"));
        assert_eq!(typed(KeyCode::Tab, Modifiers::SHIFT), one("<S-Tab>"));
        assert_eq!(one("<C-S-a>").notation(), "<C-S-a>");
        assert_eq!(one("<").notation(), "<lt>");
    }

    #[test]
    fn strokes_turn_back_into_events() {
        for s in ["j", "G", "<S-g>", "<C-h>", "<CR>", "<S-Tab>", "<M-x>", "%"] {
            let k = one(s);
            assert_eq!(Stroke::of(&k.event()), k, "{s}");
        }
        assert_eq!(one("G").event().text.as_deref(), Some("G"));
        assert_eq!(one("<S-g>").event().text.as_deref(), Some("G"));
        assert_eq!(one("<C-h>").event().text, None);
    }
}
