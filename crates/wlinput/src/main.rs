//! `wlinput`: one virtual keyboard and one virtual pointer on a wlroots
//! compositor, for `scripts/e2e.sh -d wayland` on headless sway (which has
//! no input devices of its own). wtype and wlrctl create a device per call:
//! clicks and wheel steps got lost, and every key was a focus change. This
//! keeps both devices for the whole run and reads commands on stdin, one
//! per line, answering `ok` once the compositor has them (`error: ...`
//! otherwise):
//!
//! ```text
//! move X Y              absolute, in pixels of the output (--size WxH)
//! down BTN, up BTN      1 left, 2 middle, 3 right
//! wheel N [MS]          N notches (N < 0: up), MS apart
//! keydown KEY, keyup KEY  XKB keysym names; ctrl, shift and alt are the
//!                       modifiers
//! key COMBO             KEYs joined by "+", e.g. ctrl+shift+v, Return, G
//! type MS TEXT          the rest of the line, MS between characters
//! ```

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("wlinput: Linux (Wayland) only");
    std::process::exit(2);
}

#[cfg(target_os = "linux")]
fn main() -> anyhow::Result<()> {
    linux::run()
}

#[cfg(target_os = "linux")]
mod linux {
    use std::io::{BufRead, Write};
    use std::os::fd::AsFd;
    use std::time::{Duration, Instant};

    use anyhow::{Context, Result, bail};
    use wayland_client::globals::{GlobalListContents, registry_queue_init};
    use wayland_client::protocol::{wl_keyboard, wl_pointer, wl_registry, wl_seat};
    use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, delegate_noop};
    use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
        zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1, zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
    };
    use wayland_protocols_wlr::virtual_pointer::v1::client::{
        zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1, zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1,
    };

    use crate::keymap::{Key, Keymap, Mods};

    struct State;

    impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
        fn event(
            _: &mut Self,
            _: &wl_registry::WlRegistry,
            _: wl_registry::Event,
            _: &GlobalListContents,
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
        }
    }
    delegate_noop!(State: ignore wl_seat::WlSeat);
    delegate_noop!(State: ZwlrVirtualPointerManagerV1);
    delegate_noop!(State: ZwlrVirtualPointerV1);
    delegate_noop!(State: ZwpVirtualKeyboardManagerV1);
    delegate_noop!(State: ZwpVirtualKeyboardV1);

    struct Devices {
        queue: EventQueue<State>,
        pointer: ZwlrVirtualPointerV1,
        keyboard: ZwpVirtualKeyboardV1,
        keymap: Keymap,
        /// The keymap the compositor has (sent again when keys are added).
        sent: usize,
        mods: Mods,
        size: (u32, u32),
        start: Instant,
    }

    impl Devices {
        fn time(&self) -> u32 {
            self.start.elapsed().as_millis() as u32
        }

        /// Have the compositor take what was sent.
        fn sync(&mut self) -> Result<()> {
            self.queue.roundtrip(&mut State).context("the compositor went away")?;
            Ok(())
        }

        fn send_keymap(&mut self) -> Result<()> {
            if self.sent == self.keymap.len() {
                return Ok(());
            }
            let mut text = self.keymap.text().into_bytes();
            text.push(0);
            // A file it can map; gone from the directory once sent.
            let dir = std::env::var_os("XDG_RUNTIME_DIR").map_or_else(std::env::temp_dir, Into::into);
            let path = dir.join(format!("wlinput-keymap-{}", std::process::id()));
            let mut file = std::fs::File::options().read(true).write(true).create(true).truncate(true).open(&path)?;
            file.write_all(&text)?;
            self.keyboard.keymap(wl_keyboard::KeymapFormat::XkbV1.into(), file.as_fd(), text.len() as u32);
            self.sync()?;
            let _ = std::fs::remove_file(&path);
            self.sent = self.keymap.len();
            Ok(())
        }

        fn set_mods(&mut self, mods: Mods) {
            self.mods = mods;
            self.keyboard.modifiers(mods.mask(), 0, 0, 0);
        }

        fn key_state(&mut self, key: Key, down: bool) -> Result<()> {
            self.send_keymap()?;
            let t = self.time();
            let state = if down { wl_keyboard::KeyState::Pressed } else { wl_keyboard::KeyState::Released };
            self.keyboard.key(t, key.evdev(), state.into());
            if let Some(m) = key.modifier {
                let mods = if down { self.mods.with(m) } else { self.mods.without(m) };
                self.set_mods(mods);
            }
            Ok(())
        }

        /// Press and release `key`, with `mods` held around it.
        fn tap(&mut self, key: Key, mods: Mods) -> Result<()> {
            let before = self.mods;
            for m in mods.list() {
                if !before.has(m) {
                    let k = self.keymap.modifier(m);
                    self.key_state(k, true)?;
                }
            }
            self.key_state(key, true)?;
            self.key_state(key, false)?;
            for m in mods.list() {
                if !before.has(m) {
                    let k = self.keymap.modifier(m);
                    self.key_state(k, false)?;
                }
            }
            Ok(())
        }

        fn command(&mut self, line: &str) -> Result<()> {
            let (cmd, rest) = line.split_once(' ').unwrap_or((line, ""));
            let num = |s: Option<&str>| -> Result<i64> {
                s.context("a number is missing")?.parse().with_context(|| format!("not a number in {line:?}"))
            };
            match cmd {
                "move" => {
                    let mut it = rest.split_whitespace();
                    let (x, y) = (num(it.next())?, num(it.next())?);
                    let t = self.time();
                    self.pointer.motion_absolute(t, x.max(0) as u32, y.max(0) as u32, self.size.0, self.size.1);
                    self.pointer.frame();
                }
                "down" | "up" => {
                    let code = match num(rest.split_whitespace().next())? {
                        1 => 0x110, // BTN_LEFT
                        2 => 0x112, // BTN_MIDDLE
                        3 => 0x111, // BTN_RIGHT
                        b => bail!("no button {b}"),
                    };
                    let state = if cmd == "down" {
                        wl_pointer::ButtonState::Pressed
                    } else {
                        wl_pointer::ButtonState::Released
                    };
                    let t = self.time();
                    self.pointer.button(t, code, state);
                    self.pointer.frame();
                }
                "wheel" => {
                    let mut it = rest.split_whitespace();
                    let n = num(it.next())?;
                    let pause = Duration::from_millis(it.next().map_or(Ok(0), |s| num(Some(s)))? as u64);
                    let step = n.signum() as i32;
                    for i in 0..n.unsigned_abs() {
                        if i > 0 {
                            self.sync()?;
                            std::thread::sleep(pause);
                        }
                        let t = self.time();
                        self.pointer.axis_source(wl_pointer::AxisSource::Wheel);
                        self.pointer.axis_discrete(t, wl_pointer::Axis::VerticalScroll, 15.0 * step as f64, step);
                        self.pointer.frame();
                    }
                }
                "keydown" | "keyup" => {
                    let key = self.keymap.key(rest.trim())?;
                    self.key_state(key.key, cmd == "keydown")?;
                }
                "key" => {
                    let mut mods = Mods::default();
                    let mut parts: Vec<&str> = rest.trim().split('+').collect();
                    let last = parts.pop().filter(|s| !s.is_empty()).context("no key")?;
                    for p in parts {
                        mods = mods.with(Mods::named(p).with_context(|| format!("no modifier {p:?}"))?);
                    }
                    let key = self.keymap.key(last)?;
                    self.tap(key.key, mods.union(key.mods))?;
                }
                "type" => {
                    let (ms, text) = rest.split_once(' ').unwrap_or((rest, ""));
                    let pause = Duration::from_millis(num(Some(ms))? as u64);
                    for (i, c) in text.chars().enumerate() {
                        if i > 0 && !pause.is_zero() {
                            self.sync()?;
                            std::thread::sleep(pause);
                        }
                        let key = self.keymap.char_key(c)?;
                        self.tap(key.key, key.mods)?;
                    }
                }
                _ => bail!("unknown command {cmd:?}"),
            }
            self.sync()
        }
    }

    pub fn run() -> Result<()> {
        let mut size = (0, 0);
        let mut args = std::env::args().skip(1);
        while let Some(a) = args.next() {
            match a.as_str() {
                "--size" => {
                    let s = args.next().context("--size WxH")?;
                    let (w, h) = s.split_once('x').context("--size WxH")?;
                    size = (w.parse()?, h.parse()?);
                }
                _ => bail!("usage: wlinput --size WxH (commands on stdin; see the source)"),
            }
        }
        if size.0 == 0 || size.1 == 0 {
            bail!("usage: wlinput --size WxH");
        }
        let conn = Connection::connect_to_env().context("no Wayland compositor (WAYLAND_DISPLAY)")?;
        let (globals, mut queue) = registry_queue_init::<State>(&conn)?;
        let qh = queue.handle();
        let seat: wl_seat::WlSeat = globals.bind(&qh, 1..=1, ())?;
        let pointers: ZwlrVirtualPointerManagerV1 =
            globals.bind(&qh, 1..=1, ()).context("no zwlr_virtual_pointer_manager_v1")?;
        let keyboards: ZwpVirtualKeyboardManagerV1 =
            globals.bind(&qh, 1..=1, ()).context("no zwp_virtual_keyboard_manager_v1")?;
        let pointer = pointers.create_virtual_pointer(Some(&seat), &qh, ());
        let keyboard = keyboards.create_virtual_keyboard(&seat, &qh, ());
        queue.roundtrip(&mut State)?;
        let mut dev = Devices {
            queue,
            pointer,
            keyboard,
            keymap: Keymap::new(),
            sent: 0,
            mods: Mods::default(),
            size,
            start: Instant::now(),
        };
        dev.send_keymap()?;
        let stdout = std::io::stdout();
        for line in std::io::stdin().lock().lines() {
            let line = line?;
            let answer = match dev.command(&line) {
                Ok(()) => "ok".to_owned(),
                Err(e) => format!("error: {e:#}"),
            };
            let mut out = stdout.lock();
            writeln!(out, "{answer}")?;
            out.flush()?;
        }
        Ok(())
    }
}

/// The keymap: the keys of a US keyboard at their evdev codes, symbols on
/// the shift level as there (so terminals that report the base-layout key,
/// kitty's keyboard protocol, see the real one: temzdf takes a base 'c'
/// with Ctrl for Ctrl+C), and other characters added at free codes as they
/// are typed.
mod keymap {
    use anyhow::{Result, bail};

    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct Mods(u32);

    pub const SHIFT: u32 = 1;
    pub const CTRL: u32 = 4;
    pub const ALT: u32 = 8;

    impl Mods {
        /// The real modifiers' mask (Shift, Lock, Control, Mod1, ...).
        pub fn mask(self) -> u32 {
            self.0
        }
        pub fn named(name: &str) -> Option<u32> {
            match name {
                "shift" | "Shift_L" => Some(SHIFT),
                "ctrl" | "control" | "Control_L" => Some(CTRL),
                "alt" | "Alt_L" => Some(ALT),
                _ => None,
            }
        }
        pub fn with(self, m: u32) -> Self {
            Self(self.0 | m)
        }
        pub fn without(self, m: u32) -> Self {
            Self(self.0 & !m)
        }
        pub fn union(self, o: Self) -> Self {
            Self(self.0 | o.0)
        }
        pub fn has(self, m: u32) -> bool {
            self.0 & m != 0
        }
        pub fn list(self) -> Vec<u32> {
            [SHIFT, CTRL, ALT].into_iter().filter(|&m| self.has(m)).collect()
        }
    }

    /// A key: its evdev code, and the modifier it is (if one).
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Key {
        code: u32,
        pub modifier: Option<u32>,
    }

    impl Key {
        /// The evdev code the protocol takes (XKB keycodes are 8 more).
        pub fn evdev(self) -> u32 {
            self.code
        }
    }

    /// A key and the modifiers that make it produce what was asked.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Typed {
        pub key: Key,
        pub mods: Mods,
    }

    const MODIFIERS: [(u32, &str, u32, &str); 3] =
        [(42, "Shift_L", SHIFT, "Shift"), (29, "Control_L", CTRL, "Control"), (56, "Alt_L", ALT, "Mod1")];

    /// A US keyboard: evdev code, then the keysyms without and with Shift.
    const US: &[(u32, &str, &str)] = &[
        (1, "Escape", ""),
        (2, "1", "exclam"),
        (3, "2", "at"),
        (4, "3", "numbersign"),
        (5, "4", "dollar"),
        (6, "5", "percent"),
        (7, "6", "asciicircum"),
        (8, "7", "ampersand"),
        (9, "8", "asterisk"),
        (10, "9", "parenleft"),
        (11, "0", "parenright"),
        (12, "minus", "underscore"),
        (13, "equal", "plus"),
        (14, "BackSpace", ""),
        (15, "Tab", ""),
        (16, "q", "Q"),
        (17, "w", "W"),
        (18, "e", "E"),
        (19, "r", "R"),
        (20, "t", "T"),
        (21, "y", "Y"),
        (22, "u", "U"),
        (23, "i", "I"),
        (24, "o", "O"),
        (25, "p", "P"),
        (26, "bracketleft", "braceleft"),
        (27, "bracketright", "braceright"),
        (28, "Return", ""),
        (30, "a", "A"),
        (31, "s", "S"),
        (32, "d", "D"),
        (33, "f", "F"),
        (34, "g", "G"),
        (35, "h", "H"),
        (36, "j", "J"),
        (37, "k", "K"),
        (38, "l", "L"),
        (39, "semicolon", "colon"),
        (40, "apostrophe", "quotedbl"),
        (41, "grave", "asciitilde"),
        (43, "backslash", "bar"),
        (44, "z", "Z"),
        (45, "x", "X"),
        (46, "c", "C"),
        (47, "v", "V"),
        (48, "b", "B"),
        (49, "n", "N"),
        (50, "m", "M"),
        (51, "comma", "less"),
        (52, "period", "greater"),
        (53, "slash", "question"),
        (57, "space", ""),
        (59, "F1", ""),
        (60, "F2", ""),
        (61, "F3", ""),
        (62, "F4", ""),
        (63, "F5", ""),
        (64, "F6", ""),
        (65, "F7", ""),
        (66, "F8", ""),
        (67, "F9", ""),
        (68, "F10", ""),
        (87, "F11", ""),
        (88, "F12", ""),
        (102, "Home", ""),
        (103, "Up", ""),
        (104, "Page_Up", ""),
        (105, "Left", ""),
        (106, "Right", ""),
        (107, "End", ""),
        (108, "Down", ""),
        (109, "Page_Down", ""),
        (110, "Insert", ""),
        (111, "Delete", ""),
    ];
    /// Where keys of no US keyboard go (XKB keycodes stop at 255).
    const EXTRA: std::ops::Range<u32> = 183..248;

    /// Printable ASCII other than letters and digits, with their keysym names.
    const ASCII: &[(char, &str)] = &[
        (' ', "space"),
        ('!', "exclam"),
        ('"', "quotedbl"),
        ('#', "numbersign"),
        ('$', "dollar"),
        ('%', "percent"),
        ('&', "ampersand"),
        ('\'', "apostrophe"),
        ('(', "parenleft"),
        (')', "parenright"),
        ('*', "asterisk"),
        ('+', "plus"),
        (',', "comma"),
        ('-', "minus"),
        ('.', "period"),
        ('/', "slash"),
        (':', "colon"),
        (';', "semicolon"),
        ('<', "less"),
        ('=', "equal"),
        ('>', "greater"),
        ('?', "question"),
        ('@', "at"),
        ('[', "bracketleft"),
        ('\\', "backslash"),
        (']', "bracketright"),
        ('^', "asciicircum"),
        ('_', "underscore"),
        ('`', "grave"),
        ('{', "braceleft"),
        ('|', "bar"),
        ('}', "braceright"),
        ('~', "asciitilde"),
        ('\n', "Return"),
        ('\t', "Tab"),
    ];

    pub struct Keymap {
        /// Keysyms added beyond the US keyboard, at EXTRA's codes in order.
        extra: Vec<String>,
    }

    impl Keymap {
        pub fn new() -> Self {
            Self { extra: Vec::new() }
        }

        /// How many keys the keymap has (it changes when keys are added).
        pub fn len(&self) -> usize {
            US.len() + MODIFIERS.len() + self.extra.len()
        }

        pub fn modifier(&mut self, m: u32) -> Key {
            let code = MODIFIERS.iter().find(|(_, _, v, _)| *v == m).expect("a modifier").0;
            Key { code, modifier: Some(m) }
        }

        /// The key and modifiers producing keysym `sym`: a US key (Shift
        /// for its upper symbol), else one added.
        fn sym_key(&mut self, sym: &str) -> Result<Typed> {
            let plain = |code| Typed { key: Key { code, modifier: None }, mods: Mods::default() };
            if let Some((code, ..)) = US.iter().find(|(_, lower, _)| *lower == sym) {
                return Ok(plain(*code));
            }
            if let Some((code, ..)) = US.iter().find(|(_, _, upper)| *upper == sym) {
                return Ok(Typed { key: Key { code: *code, modifier: None }, mods: Mods::default().with(SHIFT) });
            }
            let i = match self.extra.iter().position(|s| s == sym) {
                Some(i) => i,
                None => {
                    if self.extra.len() >= EXTRA.len() {
                        bail!("no free keycode for {sym:?}");
                    }
                    self.extra.push(sym.to_owned());
                    self.extra.len() - 1
                }
            };
            Ok(plain(EXTRA.start + i as u32))
        }

        /// The key for a keysym name as xdotool takes it: a modifier, a
        /// character (a capital is its letter with Shift), or any keysym.
        pub fn key(&mut self, name: &str) -> Result<Typed> {
            if name.is_empty() || name.contains(char::is_whitespace) {
                bail!("bad key name {name:?}");
            }
            if let Some(m) = Mods::named(name) {
                return Ok(Typed { key: self.modifier(m), mods: Mods::default() });
            }
            let mut chars = name.chars();
            if let (Some(c), None) = (chars.next(), chars.next()) {
                return self.char_key(c);
            }
            self.sym_key(name)
        }

        /// The key typing `c`.
        pub fn char_key(&mut self, c: char) -> Result<Typed> {
            let sym = if c.is_ascii_alphanumeric() {
                c.to_string()
            } else {
                match ASCII.iter().find(|(a, _)| *a == c) {
                    Some((_, n)) => (*n).to_owned(),
                    None => format!("U{:04X}", c as u32),
                }
            };
            self.sym_key(&sym)
        }

        /// The XKB keymap text (types and compatibility from the system's
        /// "complete" sets, as wtype does).
        pub fn text(&self) -> String {
            let mut keys: Vec<(u32, String)> = Vec::new();
            for (code, lower, upper) in US {
                let syms = if upper.is_empty() { format!("[ {lower} ]") } else { format!("[ {lower}, {upper} ]") };
                let letter = lower.len() == 1 && lower.chars().all(|c| c.is_ascii_lowercase());
                let ty = if letter {
                    "type = \"ALPHABETIC\", "
                } else if upper.is_empty() {
                    ""
                } else {
                    "type = \"TWO_LEVEL\", "
                };
                keys.push((*code, format!("{{ {ty}{syms} }}")));
            }
            for (code, name, _, _) in MODIFIERS {
                keys.push((code, format!("{{ [ {name} ] }}")));
            }
            for (i, sym) in self.extra.iter().enumerate() {
                keys.push((EXTRA.start + i as u32, format!("{{ [ {sym} ] }}")));
            }
            let mut keycodes = String::new();
            let mut symbols = String::new();
            for (code, def) in &keys {
                keycodes.push_str(&format!("    <K{code}> = {};\n", code + 8));
                symbols.push_str(&format!("    key <K{code}> {def};\n"));
            }
            for (code, _, _, map) in MODIFIERS {
                symbols.push_str(&format!("    modifier_map {map} {{ <K{code}> }};\n"));
            }
            format!(
                "xkb_keymap {{\n  xkb_keycodes \"wlinput\" {{\n    minimum = 8;\n    maximum = 255;\n{keycodes}  }};\n  \
                 xkb_types \"wlinput\" {{ include \"complete\" }};\n  \
                 xkb_compatibility \"wlinput\" {{ include \"complete\" }};\n  \
                 xkb_symbols \"wlinput\" {{\n{symbols}  }};\n}};\n"
            )
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn code(t: Typed) -> u32 {
            t.key.evdev()
        }

        #[test]
        fn keys_sit_where_a_us_keyboard_has_them() {
            let mut k = Keymap::new();
            assert_eq!(code(k.char_key('c').unwrap()), 46, "KEY_C: kitty reports it as the base key");
            let shift = Mods::default().with(SHIFT);
            assert_eq!(k.char_key('C').unwrap().mods, shift);
            assert_eq!(k.key("G").unwrap(), Typed { key: k.char_key('g').unwrap().key, mods: shift });
            let bang = k.char_key('!').unwrap();
            assert_eq!((code(bang), bang.mods), (2, shift), "Shift+1");
            assert_eq!(k.key("percent").unwrap(), k.char_key('%').unwrap());
            assert_eq!(k.key("ctrl").unwrap().key.modifier, Some(CTRL));
            assert_eq!(code(k.key("Return").unwrap()), 28);
            let before = k.len();
            let wide = k.char_key('数').unwrap();
            assert_eq!((code(wide), k.len()), (EXTRA.start, before + 1), "added as typed");
            assert_eq!(k.char_key('数').unwrap(), wide, "once");
            let text = k.text();
            assert!(text.contains("<K46> = 54;") && text.contains("[ c, C ]"), "{text}");
            assert!(text.contains("<K183> { [ U6570 ] }") && text.contains("modifier_map Control { <K29> }"));
            assert!(k.key("two words").is_err());
        }
    }
}
