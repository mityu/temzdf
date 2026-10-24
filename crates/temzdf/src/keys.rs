//! Key bindings: viewer commands, the key notation shared by the config file
//! (`[keys]`) and `--keys`, and the keymap that turns key presses into commands.
//!
//! Counts (digits before a command), the outline sidebar and the one-line input
//! keep fixed keys; see `doc/config.md`.

use std::collections::HashMap;

use anyhow::{Result, bail};
use serde::Deserialize;
use termpix::input::{KeyCode, KeyEvent, Modifiers};

/// What a key can do in the page view.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Command {
    Quit,
    /// Clear the selection, else the search, else quit.
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
    Search,
    NextMatch,
    PrevMatch,
    Copy,
    ToggleSidebar,
}

/// Default bindings, in the notation of [`Key::parse`].
const DEFAULTS: &[(Command, &[&str])] = &[
    (Command::Quit, &["q", "C-c"]),
    (Command::Cancel, &["Esc"]),
    (Command::ScrollDown, &["j", "Down"]),
    (Command::ScrollUp, &["k", "Up"]),
    (Command::ScrollLeft, &["h", "Left"]),
    (Command::ScrollRight, &["l", "Right"]),
    (Command::PageDown, &["Space", "PageDown", "C-f"]),
    (Command::PageUp, &["b", "PageUp"]),
    (Command::HalfPageDown, &["d"]),
    (Command::HalfPageUp, &["u"]),
    (Command::FirstPage, &["g", "Home"]),
    (Command::LastPage, &["G", "End"]),
    // Backspace too: multiplexers (zellij) take Ctrl-o and Alt+arrows.
    (Command::Back, &["C-o", "Backspace", "M-Left"]),
    (Command::Forward, &["M-Right"]),
    (Command::ZoomIn, &["+", "="]),
    (Command::ZoomOut, &["-"]),
    (Command::FitWidth, &["0", "w"]),
    (Command::Search, &["/"]),
    (Command::NextMatch, &["n"]),
    (Command::PrevMatch, &["N"]),
    (Command::Copy, &["y"]),
    (Command::ToggleSidebar, &["Tab"]),
];

/// A key with modifiers, as bindings match it. For characters Shift is part of
/// the character (`G`, `+`), not a modifier; lock keys never count.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Key {
    pub code: KeyCode,
    pub mods: Modifiers,
}

impl Key {
    /// Parse one key: a single character, or a name (Tab, Enter, Esc, Space,
    /// Backspace, Delete, Insert, Up, Down, Left, Right, Home, End, PageUp,
    /// PageDown, F1-F12), after any of the prefixes `C-` (Ctrl), `M-` (Alt),
    /// `S-` (Shift, for named keys) and `D-` (Super).
    pub fn parse(s: &str) -> Result<Self> {
        let mut mods = Modifiers::NONE;
        let mut rest = s;
        // A prefix needs something after it: "-" and "C--" are the minus key.
        while rest.len() > 2 {
            let m = match &rest[..2] {
                "C-" => Modifiers::CTRL,
                "M-" => Modifiers::ALT,
                "S-" => Modifiers::SHIFT,
                "D-" => Modifiers::SUPER,
                _ => break,
            };
            mods |= m;
            rest = &rest[2..];
        }
        let code = match rest {
            "Tab" => KeyCode::Tab,
            "Enter" => KeyCode::Enter,
            "Esc" => KeyCode::Escape,
            "Space" => KeyCode::Char(' '),
            "Backspace" => KeyCode::Backspace,
            "Delete" => KeyCode::Delete,
            "Insert" => KeyCode::Insert,
            "Up" => KeyCode::Up,
            "Down" => KeyCode::Down,
            "Left" => KeyCode::Left,
            "Right" => KeyCode::Right,
            "Home" => KeyCode::Home,
            "End" => KeyCode::End,
            "PageUp" => KeyCode::PageUp,
            "PageDown" => KeyCode::PageDown,
            f if f.len() > 1 && f.starts_with('F') && f[1..].parse::<u8>().is_ok_and(|n| (1..=35).contains(&n)) => {
                KeyCode::F(f[1..].parse().unwrap_or(1))
            }
            c if c.chars().count() == 1 => KeyCode::Char(c.chars().next().unwrap_or(' ')),
            other => bail!("unknown key {other:?} in {s:?}"),
        };
        if matches!(code, KeyCode::Char(_)) && mods.contains(Modifiers::SHIFT) {
            bail!("{s:?}: write the shifted character instead of S- (e.g. \"G\")");
        }
        Ok(Self { code, mods })
    }

    /// The key a press matches.
    pub fn of(ev: &KeyEvent) -> Self {
        let mut mods = ev.modifiers.without_locks();
        if matches!(ev.code, KeyCode::Char(_)) {
            mods = Modifiers::from_bits(mods.bits() & !Modifiers::SHIFT.bits());
        }
        Self { code: ev.code, mods }
    }

    /// A press of this key, with the text a terminal would report for it.
    pub fn event(self) -> KeyEvent {
        let mut ev = KeyEvent::new(self.code, self.mods);
        if let KeyCode::Char(c) = self.code
            && !self.mods.contains(Modifiers::CTRL)
        {
            ev.text = Some(c.to_string());
        }
        ev
    }
}

/// Key -> command, built from [`DEFAULTS`] and the config's `[keys]`.
#[derive(Clone, Debug)]
pub struct Keymap {
    map: HashMap<Key, Command>,
}

impl Default for Keymap {
    fn default() -> Self {
        Self::new(&HashMap::new()).expect("default bindings parse")
    }
}

impl Keymap {
    /// The defaults, with the keys of each command in `overrides` replaced by
    /// the given ones (an empty list unbinds the command). A key given in
    /// `overrides` leaves whatever default command it had.
    pub fn new(overrides: &HashMap<Command, Vec<String>>) -> Result<Self> {
        let mut map = HashMap::new();
        for &(cmd, keys) in DEFAULTS {
            if overrides.contains_key(&cmd) {
                continue;
            }
            for k in keys {
                map.insert(Key::parse(k)?, cmd);
            }
        }
        let mut seen = HashMap::new();
        for (&cmd, keys) in overrides {
            for k in keys {
                let key = Key::parse(k)?;
                if let Some(other) = seen.insert(key, cmd)
                    && other != cmd
                {
                    bail!("{k:?} is bound to both {other:?} and {cmd:?}");
                }
                map.insert(key, cmd);
            }
        }
        Ok(Self { map })
    }

    pub fn get(&self, ev: &KeyEvent) -> Option<Command> {
        self.map.get(&Key::of(ev)).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(code: KeyCode, mods: Modifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    #[test]
    fn parses_the_notation() {
        assert_eq!(Key::parse("j").unwrap(), Key { code: KeyCode::Char('j'), mods: Modifiers::NONE });
        assert_eq!(Key::parse("C-f").unwrap(), Key { code: KeyCode::Char('f'), mods: Modifiers::CTRL });
        assert_eq!(
            Key::parse("C-M-Left").unwrap(),
            Key { code: KeyCode::Left, mods: Modifiers::CTRL | Modifiers::ALT }
        );
        assert_eq!(Key::parse("-").unwrap().code, KeyCode::Char('-'));
        assert_eq!(Key::parse("C--").unwrap(), Key { code: KeyCode::Char('-'), mods: Modifiers::CTRL });
        assert_eq!(Key::parse("F5").unwrap().code, KeyCode::F(5));
        assert_eq!(Key::parse("S-Tab").unwrap(), Key { code: KeyCode::Tab, mods: Modifiers::SHIFT });
        assert!(Key::parse("S-g").is_err());
        assert!(Key::parse("Hyper").is_err());
    }

    #[test]
    fn shift_is_part_of_characters_and_locks_are_ignored() {
        let m = Keymap::default();
        assert_eq!(m.get(&press(KeyCode::Char('G'), Modifiers::SHIFT)), Some(Command::LastPage));
        assert_eq!(m.get(&press(KeyCode::Char('j'), Modifiers::NUM_LOCK)), Some(Command::ScrollDown));
        assert_eq!(m.get(&press(KeyCode::Char('j'), Modifiers::CTRL)), None);
        assert_eq!(m.get(&press(KeyCode::Left, Modifiers::ALT)), Some(Command::Back));
        assert_eq!(m.get(&press(KeyCode::Left, Modifiers::NONE)), Some(Command::ScrollLeft));
    }

    #[test]
    fn overrides_replace_a_commands_keys_and_steal_keys() {
        let overrides = HashMap::from([
            (Command::Quit, vec!["Q".to_string()]),
            (Command::Search, vec!["f".to_string(), "/".to_string()]),
            (Command::Copy, vec![]),
        ]);
        let m = Keymap::new(&overrides).unwrap();
        assert_eq!(m.get(&press(KeyCode::Char('q'), Modifiers::NONE)), None);
        assert_eq!(m.get(&press(KeyCode::Char('Q'), Modifiers::SHIFT)), Some(Command::Quit));
        assert_eq!(m.get(&press(KeyCode::Char('f'), Modifiers::NONE)), Some(Command::Search));
        assert_eq!(m.get(&press(KeyCode::Char('y'), Modifiers::NONE)), None);
        // Untouched commands keep their defaults.
        assert_eq!(m.get(&press(KeyCode::Char('f'), Modifiers::CTRL)), Some(Command::PageDown));
    }

    #[test]
    fn a_key_bound_twice_is_an_error() {
        let overrides = HashMap::from([(Command::Quit, vec!["x".to_string()]), (Command::Copy, vec!["x".to_string()])]);
        assert!(Keymap::new(&overrides).is_err());
    }
}
