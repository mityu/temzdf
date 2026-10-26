//! Terminal input parser: raw bytes from the tty to [`Event`]s.
//!
//! Handles SGR mouse reports (cell or pixel coordinates), the kitty keyboard
//! protocol and legacy key encodings, bracketed paste, focus, in-band resize
//! (mode 2048), kitty graphics replies (APC `_G`) and the query replies we send
//! during probing (DA1, DECRPM, XTVERSION, window/cell size, kitty keyboard flags).
//!
//! The parser is a pure state machine: feed it bytes with [`Parser::feed`], and
//! call [`Parser::flush`] when no more bytes arrived for a short while so a lone
//! `ESC` is reported as the Escape key.

pub use termpix_core::input::*;

/// Terminal size from an in-band resize report (mode 2048).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResizeReport {
    pub rows: u16,
    pub cols: u16,
    pub height_px: u32,
    pub width_px: u32,
}

impl ResizeReport {
    /// The geometry it reports, if complete.
    pub fn geometry(&self) -> Option<crate::geom::Geometry> {
        if self.rows == 0 || self.cols == 0 || self.width_px == 0 || self.height_px == 0 {
            return None;
        }
        let cell = crate::geom::CellSize {
            width: self.width_px / self.cols as u32,
            height: self.height_px / self.rows as u32,
        };
        Some(crate::geom::Geometry { cols: self.cols, rows: self.rows, cell })
    }
}

/// A kitty graphics protocol reply: `ESC _G <keys> ; <message> ESC \`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphicsReply {
    pub image_id: Option<u32>,
    pub image_number: Option<u32>,
    pub placement_id: Option<u32>,
    /// `OK`, or an error such as `ENOENT:...`.
    pub message: String,
}

impl GraphicsReply {
    pub fn is_ok(&self) -> bool {
        self.message == "OK"
    }

    /// The error code (`ENOENT`, `EINVAL`, ...) if this is an error reply.
    pub fn error_code(&self) -> Option<&str> {
        if self.is_ok() { None } else { Some(self.message.split(':').next().unwrap_or("")) }
    }
}

/// DECRPM mode state (`CSI ? mode ; state $ y`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModeState {
    NotRecognized,
    Set,
    Reset,
    PermanentlySet,
    PermanentlyReset,
}

impl ModeState {
    pub fn is_supported(self) -> bool {
        !matches!(self, Self::NotRecognized | Self::PermanentlyReset)
    }
}

/// Replies to queries sent by the application.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reply {
    /// Primary device attributes (`CSI ? ... c`): the parameters.
    DeviceAttributes(Vec<u32>),
    /// DECRPM for a private mode.
    Mode { mode: u32, state: ModeState },
    /// XTVERSION (`DCS > | text ST`).
    Version(String),
    /// `CSI 16 t` reply: cell size in pixels.
    CellSize { width: u32, height: u32 },
    /// `CSI 14 t` reply: text area size in pixels.
    WindowSize { width: u32, height: u32 },
    /// `CSI ? u` reply: active kitty keyboard flags.
    KeyboardFlags(u32),
    /// An OSC reply (e.g. colour queries), without the introducer and terminator.
    Osc(Vec<u8>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Key(KeyEvent),
    Mouse(MouseEvent),
    Paste(String),
    Focus(bool),
    Resize(ResizeReport),
    Graphics(GraphicsReply),
    Reply(Reply),
    /// A complete escape sequence we did not recognise.
    Unknown(Vec<u8>),
}

const ESC: u8 = 0x1b;
const PASTE_END: &[u8] = b"\x1b[201~";

/// Incremental input parser. See the module documentation.
#[derive(Debug, Default)]
pub struct Parser {
    buf: Vec<u8>,
    pixel_mouse: bool,
    paste: Option<Vec<u8>>,
    /// Flushes in a row that found a sequence begun and not finished.
    stalls: u32,
}

/// Flushes a begun sequence waits through (at the 25 ms the reader waits
/// between them, about a second) before it is dropped.
const MAX_STALLS: u32 = 40;

enum Step {
    /// Consumed `n` bytes, maybe producing an event.
    Done(usize, Option<Event>),
    /// Need more bytes to decide.
    Incomplete,
}

impl Parser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether SGR mouse coordinates are pixels (mode 1016 enabled).
    pub fn set_pixel_mouse(&mut self, enabled: bool) {
        self.pixel_mouse = enabled;
    }

    /// Whether bytes are buffered waiting for the rest of a sequence.
    pub fn has_pending(&self) -> bool {
        !self.buf.is_empty() || self.paste.is_some()
    }

    /// Feed bytes and return every event that is complete.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Event> {
        if !bytes.is_empty() {
            self.stalls = 0;
        }
        let mut buf = std::mem::take(&mut self.buf);
        buf.extend_from_slice(bytes);
        let mut events = Vec::new();
        let mut pos = 0;
        while pos < buf.len() {
            if let Some(paste) = &mut self.paste {
                match find(&buf[pos..], PASTE_END) {
                    Some(end) => {
                        paste.extend_from_slice(&buf[pos..pos + end]);
                        let text = String::from_utf8_lossy(paste).into_owned();
                        self.paste = None;
                        events.push(Event::Paste(text));
                        pos += end + PASTE_END.len();
                    }
                    None => {
                        // Keep a tail that may be the start of the terminator.
                        let rest = &buf[pos..];
                        let keep = partial_suffix(rest, PASTE_END);
                        paste.extend_from_slice(&rest[..rest.len() - keep]);
                        pos = buf.len() - keep;
                        break;
                    }
                }
                continue;
            }
            match self.step(&buf[pos..]) {
                Step::Done(n, event) => {
                    pos += n;
                    if let Some(event) = event {
                        events.push(event);
                    }
                }
                Step::Incomplete => break,
            }
        }
        buf.drain(..pos);
        self.buf = buf;
        events
    }

    /// Interpret whatever is buffered as complete input. Call this when no bytes
    /// arrived for a short while (e.g. 25 ms): a lone `ESC` becomes the Escape key
    /// and `ESC x` becomes Alt+x.
    ///
    /// A control sequence begun and not finished (`ESC [` and more, as a
    /// mouse report split over a slow link) is not keys: it waits for the
    /// rest, and is dropped if that never comes.
    pub fn flush(&mut self) -> Vec<Event> {
        let mut events = Vec::new();
        if self.paste.is_some() {
            // An unterminated paste: keep waiting, pastes can be large and slow.
            return events;
        }
        let begun = self.buf.len() > 2 && self.buf[0] == ESC && matches!(self.buf[1], b'[' | b']' | b'P' | b'_');
        if begun {
            self.stalls += 1;
            if self.stalls < MAX_STALLS {
                return events;
            }
            self.stalls = 0;
            return vec![Event::Unknown(std::mem::take(&mut self.buf))];
        }
        let buf = std::mem::take(&mut self.buf);
        let mut pos = 0;
        while pos < buf.len() {
            if buf[pos] == ESC {
                if pos + 1 == buf.len() {
                    events.push(key(KeyCode::Escape, Modifiers::NONE));
                    pos += 1;
                    continue;
                }
                let mut rest = self.feed(&buf[pos + 1..]);
                if let Some(Event::Key(k)) = rest.first_mut() {
                    k.modifiers |= Modifiers::ALT;
                } else {
                    events.push(key(KeyCode::Escape, Modifiers::NONE));
                }
                events.append(&mut rest);
                events.append(&mut self.flush());
                return events;
            }
            let mut rest = self.feed(&buf[pos..]);
            events.append(&mut rest);
            // Anything still buffered is an incomplete sequence or UTF-8 char.
            let leftover = std::mem::take(&mut self.buf);
            if !leftover.is_empty() {
                events.push(Event::Unknown(leftover));
            }
            return events;
        }
        events
    }

    fn step(&mut self, b: &[u8]) -> Step {
        if b[0] != ESC {
            return text_key(b);
        }
        if b.len() < 2 {
            return Step::Incomplete;
        }
        match b[1] {
            b'[' => self.csi(b),
            b'O' => ss3(b),
            b'_' => string_seq(b, 2).map(|(n, body)| Step::Done(n, Some(apc(body)))).unwrap_or(Step::Incomplete),
            b']' => string_seq(b, 2)
                .map(|(n, body)| Step::Done(n, Some(Event::Reply(Reply::Osc(body.to_vec())))))
                .unwrap_or(Step::Incomplete),
            b'P' => string_seq(b, 2).map(|(n, body)| Step::Done(n, Some(dcs(body)))).unwrap_or(Step::Incomplete),
            ESC => Step::Done(1, Some(key(KeyCode::Escape, Modifiers::NONE))),
            _ => match text_key(&b[1..]) {
                Step::Done(n, Some(Event::Key(mut k))) => {
                    k.modifiers |= Modifiers::ALT;
                    Step::Done(n + 1, Some(Event::Key(k)))
                }
                Step::Done(n, other) => Step::Done(n + 1, other),
                Step::Incomplete => Step::Incomplete,
            },
        }
    }

    fn csi(&mut self, b: &[u8]) -> Step {
        // ESC [ <params 0x30-0x3f>* <intermediates 0x20-0x2f>* <final 0x40-0x7e>
        let mut i = 2;
        while i < b.len() && (0x30..=0x3f).contains(&b[i]) {
            i += 1;
        }
        let params_end = i;
        while i < b.len() && (0x20..=0x2f).contains(&b[i]) {
            i += 1;
        }
        if i >= b.len() {
            return Step::Incomplete;
        }
        let fin = b[i];
        if !(0x40..=0x7e).contains(&fin) {
            // Malformed: drop the introducer and resynchronise.
            return Step::Done(2, Some(Event::Unknown(b[..2].to_vec())));
        }
        let params = &b[2..params_end];
        let inter = &b[params_end..i];
        let len = i + 1;
        if params == b"200" && fin == b'~' {
            self.paste = Some(Vec::new());
            return Step::Done(len, None);
        }
        let event = self.csi_event(params, inter, fin).unwrap_or_else(|| Event::Unknown(b[..len].to_vec()));
        Step::Done(len, Some(event))
    }

    fn csi_event(&self, params: &[u8], inter: &[u8], fin: u8) -> Option<Event> {
        let s = std::str::from_utf8(params).ok()?;
        if let Some(rest) = s.strip_prefix('<') {
            if inter.is_empty() && (fin == b'M' || fin == b'm') {
                return self.sgr_mouse(rest, fin == b'm');
            }
            return None;
        }
        if let Some(rest) = s.strip_prefix('?') {
            return match (inter, fin) {
                (b"", b'c') => Some(Event::Reply(Reply::DeviceAttributes(numbers(rest)))),
                (b"", b'u') => Some(Event::Reply(Reply::KeyboardFlags(rest.parse().ok()?))),
                (b"$", b'y') => {
                    let n = numbers(rest);
                    let state = match n.get(1)? {
                        0 => ModeState::NotRecognized,
                        1 => ModeState::Set,
                        2 => ModeState::Reset,
                        3 => ModeState::PermanentlySet,
                        4 => ModeState::PermanentlyReset,
                        _ => return None,
                    };
                    Some(Event::Reply(Reply::Mode { mode: *n.first()?, state }))
                }
                _ => None,
            };
        }
        if !inter.is_empty() || s.starts_with(['<', '=', '>']) {
            return None;
        }
        let fields: Vec<&str> = s.split(';').collect();
        match fin {
            b'u' => kitty_key(&fields),
            b'~' => tilde_key(&fields),
            b'A' | b'B' | b'C' | b'D' | b'E' | b'F' | b'H' | b'P' | b'Q' | b'S' => {
                let code = match fin {
                    b'A' => KeyCode::Up,
                    b'B' => KeyCode::Down,
                    b'C' => KeyCode::Right,
                    b'D' => KeyCode::Left,
                    b'E' => KeyCode::Begin,
                    b'F' => KeyCode::End,
                    b'H' => KeyCode::Home,
                    b'P' => KeyCode::F(1),
                    b'Q' => KeyCode::F(2),
                    _ => KeyCode::F(4),
                };
                let (modifiers, kind) = mods_and_kind(fields.get(1).copied());
                Some(Event::Key(KeyEvent { code, modifiers, kind, text: None }))
            }
            // `CSI R` is F3 only in the unambiguous forms; `CSI row;col R` is a cursor report.
            b'R' if s.is_empty() || s.starts_with("1;") => {
                let (modifiers, kind) = mods_and_kind(fields.get(1).copied());
                Some(Event::Key(KeyEvent { code: KeyCode::F(3), modifiers, kind, text: None }))
            }
            b'Z' => Some(key(KeyCode::Tab, Modifiers::SHIFT)),
            b'I' if s.is_empty() => Some(Event::Focus(true)),
            b'O' if s.is_empty() => Some(Event::Focus(false)),
            b't' => {
                let n = numbers(s);
                match n.as_slice() {
                    [48, rows, cols, h, w] => Some(Event::Resize(ResizeReport {
                        rows: (*rows).try_into().ok()?,
                        cols: (*cols).try_into().ok()?,
                        height_px: *h,
                        width_px: *w,
                    })),
                    [6, h, w] => Some(Event::Reply(Reply::CellSize { width: *w, height: *h })),
                    [4, h, w] => Some(Event::Reply(Reply::WindowSize { width: *w, height: *h })),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    fn sgr_mouse(&self, s: &str, release: bool) -> Option<Event> {
        let mut it = s.split(';');
        let b: u32 = it.next()?.parse().ok()?;
        // Pixel coordinates can be negative in some terminals when the pointer is
        // outside the window during a drag.
        let x: i32 = it.next()?.parse().ok()?;
        let y: i32 = it.next()?.parse().ok()?;
        let mut modifiers = Modifiers::NONE;
        if b & 4 != 0 {
            modifiers |= Modifiers::SHIFT;
        }
        if b & 8 != 0 {
            modifiers |= Modifiers::ALT;
        }
        if b & 16 != 0 {
            modifiers |= Modifiers::CTRL;
        }
        let motion = b & 32 != 0;
        let button_bits = (b & 3) | ((b >> 6) << 2);
        let kind = if b & 256 != 0 {
            MouseEventKind::Leave
        } else if b & 64 != 0 && b & 128 == 0 {
            MouseEventKind::Scroll(match b & 3 {
                0 => ScrollDirection::Up,
                1 => ScrollDirection::Down,
                2 => ScrollDirection::Left,
                _ => ScrollDirection::Right,
            })
        } else {
            let button = match button_bits {
                0 => Some(MouseButton::Left),
                1 => Some(MouseButton::Middle),
                2 => Some(MouseButton::Right),
                3 => None,
                n => Some(MouseButton::Other(n as u8)),
            };
            match (motion, release, button) {
                (true, _, Some(btn)) => MouseEventKind::Drag(btn),
                (true, _, None) => MouseEventKind::Move,
                (false, true, Some(btn)) => MouseEventKind::Release(btn),
                (false, false, Some(btn)) => MouseEventKind::Press(btn),
                (false, _, None) => return None,
            }
        };
        let unit = if self.pixel_mouse { CoordUnit::Pixel } else { CoordUnit::Cell };
        // Cell coordinates are one-based; SGR-pixel coordinates are zero-based
        // (verified in kitty 0.49 and Ghostty 1.3: a click at window pixel 300
        // reports 300).
        let base = if self.pixel_mouse { 0 } else { 1 };
        Some(Event::Mouse(MouseEvent { kind, x: x - base, y: y - base, unit, modifiers }))
    }
}

fn key(code: KeyCode, modifiers: Modifiers) -> Event {
    Event::Key(KeyEvent::new(code, modifiers))
}

/// A plain byte or UTF-8 character (legacy encoding).
fn text_key(b: &[u8]) -> Step {
    let c = b[0];
    let ev = match c {
        b'\r' | b'\n' => key(KeyCode::Enter, Modifiers::NONE),
        b'\t' => key(KeyCode::Tab, Modifiers::NONE),
        0x7f | 0x08 => key(KeyCode::Backspace, Modifiers::NONE),
        0x00 => key(KeyCode::Char(' '), Modifiers::CTRL),
        0x01..=0x1a => key(KeyCode::Char((b'a' + c - 1) as char), Modifiers::CTRL),
        0x1c..=0x1f => key(KeyCode::Char((b'4' + c - 0x1c) as char), Modifiers::CTRL),
        0x20..=0x7e => {
            let ch = c as char;
            let mods = if ch.is_ascii_uppercase() { Modifiers::SHIFT } else { Modifiers::NONE };
            Event::Key(KeyEvent {
                code: KeyCode::Char(ch),
                modifiers: mods,
                kind: KeyEventKind::Press,
                text: Some(ch.to_string()),
            })
        }
        _ => {
            let need = match c {
                0xc0..=0xdf => 2,
                0xe0..=0xef => 3,
                0xf0..=0xf7 => 4,
                _ => return Step::Done(1, Some(Event::Unknown(vec![c]))),
            };
            if b.len() < need {
                return Step::Incomplete;
            }
            return match std::str::from_utf8(&b[..need]) {
                Ok(s) => {
                    let ch = s.chars().next().unwrap_or('\u{fffd}');
                    Step::Done(
                        need,
                        Some(Event::Key(KeyEvent {
                            code: KeyCode::Char(ch),
                            modifiers: Modifiers::NONE,
                            kind: KeyEventKind::Press,
                            text: Some(s.to_owned()),
                        })),
                    )
                }
                Err(_) => Step::Done(1, Some(Event::Unknown(vec![c]))),
            };
        }
    };
    Step::Done(1, Some(ev))
}

fn ss3(b: &[u8]) -> Step {
    if b.len() < 3 {
        return Step::Incomplete;
    }
    let code = match b[2] {
        b'A' => KeyCode::Up,
        b'B' => KeyCode::Down,
        b'C' => KeyCode::Right,
        b'D' => KeyCode::Left,
        b'H' => KeyCode::Home,
        b'F' => KeyCode::End,
        b'P' => KeyCode::F(1),
        b'Q' => KeyCode::F(2),
        b'R' => KeyCode::F(3),
        b'S' => KeyCode::F(4),
        _ => return Step::Done(3, Some(Event::Unknown(b[..3].to_vec()))),
    };
    Step::Done(3, Some(key(code, Modifiers::NONE)))
}

/// A string sequence (APC, OSC, DCS) terminated by ST (`ESC \`) or BEL.
/// Returns the total length and the body.
fn string_seq(b: &[u8], start: usize) -> Option<(usize, &[u8])> {
    let mut i = start;
    while i < b.len() {
        match b[i] {
            0x07 => return Some((i + 1, &b[start..i])),
            ESC if i + 1 < b.len() && b[i + 1] == b'\\' => return Some((i + 2, &b[start..i])),
            _ => i += 1,
        }
    }
    None
}

fn apc(body: &[u8]) -> Event {
    let Some(rest) = body.strip_prefix(b"G") else {
        return Event::Unknown(body.to_vec());
    };
    let text = String::from_utf8_lossy(rest);
    let (keys, message) = text.split_once(';').unwrap_or((&text, ""));
    let mut reply =
        GraphicsReply { image_id: None, image_number: None, placement_id: None, message: message.to_owned() };
    for kv in keys.split(',') {
        let Some((k, v)) = kv.split_once('=') else { continue };
        let v = v.parse().ok();
        match k {
            "i" => reply.image_id = v,
            "I" => reply.image_number = v,
            "p" => reply.placement_id = v,
            _ => {}
        }
    }
    Event::Graphics(reply)
}

fn dcs(body: &[u8]) -> Event {
    match body.strip_prefix(b">|") {
        Some(v) => Event::Reply(Reply::Version(String::from_utf8_lossy(v).into_owned())),
        None => Event::Unknown(body.to_vec()),
    }
}

/// Parse `a;b;c` into numbers, treating empty or sub-parameter fields leniently.
fn numbers(s: &str) -> Vec<u32> {
    if s.is_empty() {
        return Vec::new();
    }
    s.split(';').map(|f| f.split(':').next().unwrap_or("").parse().unwrap_or(0)).collect()
}

/// The `mods[:event]` field of kitty/xterm key sequences.
fn mods_and_kind(field: Option<&str>) -> (Modifiers, KeyEventKind) {
    let Some(field) = field else { return (Modifiers::NONE, KeyEventKind::Press) };
    let mut it = field.split(':');
    let m: u32 = it.next().and_then(|v| v.parse().ok()).unwrap_or(1);
    let modifiers = Modifiers::from_bits(m.saturating_sub(1) as u8);
    let kind = match it.next().and_then(|v| v.parse::<u32>().ok()) {
        Some(2) => KeyEventKind::Repeat,
        Some(3) => KeyEventKind::Release,
        _ => KeyEventKind::Press,
    };
    (modifiers, kind)
}

/// `CSI code[:shifted[:base]] ; mods[:event] ; text u`
fn kitty_key(fields: &[&str]) -> Option<Event> {
    let mut codes = fields.first()?.split(':');
    let code: u32 = codes.next()?.parse().ok()?;
    // Alternate keys (flag 4): the shifted key, then the base-layout key.
    let shifted = codes.next().and_then(|c| c.parse::<u32>().ok()).and_then(char::from_u32);
    let (modifiers, kind) = mods_and_kind(fields.get(1).copied());
    let text = fields
        .get(2)
        .and_then(|t| t.split(':').map(|c| c.parse::<u32>().ok().and_then(char::from_u32)).collect::<Option<String>>());
    let code = match code {
        13 => KeyCode::Enter,
        9 => KeyCode::Tab,
        127 | 8 => KeyCode::Backspace,
        27 => KeyCode::Escape,
        57358 => KeyCode::CapsLock,
        57359 => KeyCode::ScrollLock,
        57360 => KeyCode::NumLock,
        57361 => KeyCode::PrintScreen,
        57362 => KeyCode::Pause,
        57363 => KeyCode::Menu,
        57376..=57398 => KeyCode::F((code - 57376 + 13) as u8),
        // The keypad (flag 8 reports it apart): as the keys it stands for.
        57399..=57408 => KeyCode::Char(char::from(b'0' + (code - 57399) as u8)),
        57409 => KeyCode::Char('.'),
        57410 => KeyCode::Char('/'),
        57411 => KeyCode::Char('*'),
        57412 => KeyCode::Char('-'),
        57413 => KeyCode::Char('+'),
        57414 => KeyCode::Enter,
        57415 => KeyCode::Char('='),
        57416 => KeyCode::Char(','),
        57417 => KeyCode::Left,
        57418 => KeyCode::Right,
        57419 => KeyCode::Up,
        57420 => KeyCode::Down,
        57421 => KeyCode::PageUp,
        57422 => KeyCode::PageDown,
        57423 => KeyCode::Home,
        57424 => KeyCode::End,
        57425 => KeyCode::Insert,
        57426 => KeyCode::Delete,
        57427 => KeyCode::Begin,
        57441..=57454 => KeyCode::Modifier(match code {
            57441 => ModifierKey::LeftShift,
            57442 => ModifierKey::LeftCtrl,
            57443 => ModifierKey::LeftAlt,
            57444 => ModifierKey::LeftSuper,
            57445 => ModifierKey::LeftHyper,
            57446 => ModifierKey::LeftMeta,
            57447 => ModifierKey::RightShift,
            57448 => ModifierKey::RightCtrl,
            57449 => ModifierKey::RightAlt,
            57450 => ModifierKey::RightSuper,
            57451 => ModifierKey::RightHyper,
            57452 => ModifierKey::RightMeta,
            57453 => ModifierKey::IsoLevel3Shift,
            _ => ModifierKey::IsoLevel5Shift,
        }),
        57344..=63743 => KeyCode::Other(code),
        _ => {
            let base = char::from_u32(code)?;
            // The protocol reports the unshifted key plus Shift. Report the character
            // actually produced, like legacy input does: from the alternate key, or
            // failing that from the associated text. The text also counts without
            // Shift in the modifiers: WezTerm leaves Shift out of the press of a
            // shifted character in some cases (`CSI 103;;71u` for G).
            let single_text = text.as_deref().and_then(|t| {
                let mut it = t.chars();
                it.next().filter(|_| it.next().is_none())
            });
            let locks = Modifiers::from_bits(Modifiers::CAPS_LOCK.bits() | Modifiers::NUM_LOCK.bits());
            let only_shift =
                Modifiers::from_bits(modifiers.bits() & !(Modifiers::SHIFT.bits() | locks.bits())) == Modifiers::NONE;
            match (modifiers.contains(Modifiers::SHIFT), shifted, single_text) {
                (true, Some(c), _) => KeyCode::Char(c),
                (_, _, Some(c)) if only_shift => KeyCode::Char(c),
                _ => KeyCode::Char(base),
            }
        }
    };
    Some(Event::Key(KeyEvent { code, modifiers, kind, text }))
}

/// `CSI number ; mods[:event] ~`
fn tilde_key(fields: &[&str]) -> Option<Event> {
    let n: u32 = fields.first()?.parse().ok()?;
    let code = match n {
        1 | 7 => KeyCode::Home,
        2 => KeyCode::Insert,
        3 => KeyCode::Delete,
        4 | 8 => KeyCode::End,
        5 => KeyCode::PageUp,
        6 => KeyCode::PageDown,
        11..=15 => KeyCode::F((n - 10) as u8),
        17..=21 => KeyCode::F((n - 11) as u8),
        23 | 24 => KeyCode::F((n - 12) as u8),
        57427 => KeyCode::Begin,
        _ => return None,
    };
    let (modifiers, kind) = mods_and_kind(fields.get(1).copied());
    Some(Event::Key(KeyEvent { code, modifiers, kind, text: None }))
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Length of the longest suffix of `hay` that is a proper prefix of `needle`.
fn partial_suffix(hay: &[u8], needle: &[u8]) -> usize {
    (1..needle.len().min(hay.len() + 1)).rev().find(|&n| hay.ends_with(&needle[..n])).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(bytes: &[u8]) -> Vec<Event> {
        Parser::new().feed(bytes)
    }

    fn k(code: KeyCode, modifiers: Modifiers) -> Event {
        key(code, modifiers)
    }

    #[test]
    fn plain_and_utf8_text() {
        let ev = parse("aあ".as_bytes());
        assert_eq!(ev.len(), 2);
        assert!(matches!(&ev[1], Event::Key(KeyEvent { code: KeyCode::Char('あ'), .. })));
    }

    #[test]
    fn utf8_split_across_feeds() {
        let mut p = Parser::new();
        let bytes = "あ".as_bytes();
        assert!(p.feed(&bytes[..2]).is_empty());
        assert!(p.has_pending());
        let ev = p.feed(&bytes[2..]);
        assert!(matches!(&ev[0], Event::Key(KeyEvent { code: KeyCode::Char('あ'), .. })));
    }

    #[test]
    fn legacy_controls() {
        assert_eq!(parse(b"\x01"), vec![k(KeyCode::Char('a'), Modifiers::CTRL)]);
        assert_eq!(parse(b"\r"), vec![k(KeyCode::Enter, Modifiers::NONE)]);
        assert_eq!(parse(b"\x7f"), vec![k(KeyCode::Backspace, Modifiers::NONE)]);
        assert_eq!(parse(b"\x1b[A"), vec![k(KeyCode::Up, Modifiers::NONE)]);
        assert_eq!(parse(b"\x1bOP"), vec![k(KeyCode::F(1), Modifiers::NONE)]);
        assert_eq!(parse(b"\x1b[1;5C"), vec![k(KeyCode::Right, Modifiers::CTRL)]);
        assert_eq!(parse(b"\x1b[5~"), vec![k(KeyCode::PageUp, Modifiers::NONE)]);
        assert_eq!(parse(b"\x1b[Z"), vec![k(KeyCode::Tab, Modifiers::SHIFT)]);
    }

    #[test]
    fn lone_escape_needs_flush() {
        let mut p = Parser::new();
        assert!(p.feed(b"\x1b").is_empty());
        assert_eq!(p.flush(), vec![k(KeyCode::Escape, Modifiers::NONE)]);
        assert!(!p.has_pending());
    }

    #[test]
    fn a_sequence_split_by_a_pause_is_not_keys() {
        // A mouse report cut in two by a slow link (SSH): the reader's
        // 25 ms flush comes in between.
        let mut p = Parser::new();
        assert!(p.feed(b"\x1b[<0;10").is_empty());
        assert!(p.flush().is_empty(), "begun: wait for the rest");
        let ev = p.feed(b";5M");
        assert!(
            matches!(ev[..], [Event::Mouse(MouseEvent { kind: MouseEventKind::Press(MouseButton::Left), .. })]),
            "{ev:?}"
        );
        // One that never ends is dropped, not typed.
        assert!(p.feed(b"\x1b[<0;1").is_empty());
        let flushed: Vec<Event> = (0..MAX_STALLS).flat_map(|_| p.flush()).collect();
        assert!(matches!(flushed[..], [Event::Unknown(_)]), "{flushed:?}");
        assert!(!p.has_pending());
        // Alt+[ is ESC [ alone.
        assert!(p.feed(b"\x1b[").is_empty());
        assert!(
            matches!(&p.flush()[..], [Event::Key(KeyEvent { code: KeyCode::Char('['), modifiers, .. })] if *modifiers == Modifiers::ALT)
        );
    }

    #[test]
    fn alt_prefix() {
        let ev = parse(b"\x1bx");
        assert!(
            matches!(&ev[0], Event::Key(KeyEvent { code: KeyCode::Char('x'), modifiers, .. }) if *modifiers == Modifiers::ALT)
        );
    }

    #[test]
    fn kitty_keys() {
        // Ctrl+a press, with flags 8 (all keys as escapes)
        assert_eq!(parse(b"\x1b[97;5u"), vec![k(KeyCode::Char('a'), Modifiers::CTRL)]);
        // Escape and Enter as CSI u
        assert_eq!(parse(b"\x1b[27u"), vec![k(KeyCode::Escape, Modifiers::NONE)]);
        assert_eq!(parse(b"\x1b[13u"), vec![k(KeyCode::Enter, Modifiers::NONE)]);
        // Release of left control (modifier-only key), ctrl bit set while it is down
        let ev = parse(b"\x1b[57442;5:3u");
        assert_eq!(
            ev,
            vec![Event::Key(KeyEvent {
                code: KeyCode::Modifier(ModifierKey::LeftCtrl),
                modifiers: Modifiers::CTRL,
                kind: KeyEventKind::Release,
                text: None
            })]
        );
        // Shifted key with alternate codes and associated text: the produced character
        let ev = parse(b"\x1b[97:65;2;65u");
        assert_eq!(
            ev,
            vec![Event::Key(KeyEvent {
                code: KeyCode::Char('A'),
                modifiers: Modifiers::SHIFT,
                kind: KeyEventKind::Press,
                text: Some("A".into())
            })]
        );
        // Shift without alternate keys (flag 4 off) falls back to the associated text;
        // with neither, the base key is kept.
        assert!(matches!(&parse(b"\x1b[103;2;71u")[0], Event::Key(KeyEvent { code: KeyCode::Char('G'), .. })));
        assert!(matches!(&parse(b"\x1b[103;2u")[0], Event::Key(KeyEvent { code: KeyCode::Char('g'), .. })));
        // The text wins even when Shift is missing (WezTerm on macOS), but not
        // with Ctrl or Alt, whose text is not what a binding means.
        assert!(matches!(&parse(b"\x1b[103;;71u")[0], Event::Key(KeyEvent { code: KeyCode::Char('G'), .. })));
        assert!(matches!(&parse(b"\x1b[102;3;402u")[0], Event::Key(KeyEvent { code: KeyCode::Char('f'), .. })));
        // Shifted digit row: Shift+3 on a US layout is '#'
        assert!(matches!(&parse(b"\x1b[51:35;2u")[0], Event::Key(KeyEvent { code: KeyCode::Char('#'), .. })));
        // The keypad, as the keys it stands for (KP_7 with Num Lock, KP_Enter,
        // KP_Left with Num Lock off).
        let code = |b: &[u8]| match &parse(b)[0] {
            Event::Key(k) => k.code,
            other => panic!("{other:?}"),
        };
        assert_eq!(code(b"\x1b[57406;129;55u"), KeyCode::Char('7'));
        assert_eq!(code(b"\x1b[57414u"), KeyCode::Enter);
        assert_eq!(code(b"\x1b[57413u"), KeyCode::Char('+'));
        assert_eq!(code(b"\x1b[57417u"), KeyCode::Left);
        // Repeat of an arrow key
        let ev = parse(b"\x1b[1;1:2A");
        assert!(matches!(&ev[0], Event::Key(KeyEvent { code: KeyCode::Up, kind: KeyEventKind::Repeat, .. })));
    }

    #[test]
    fn sgr_mouse_cells_and_pixels() {
        let ev = parse(b"\x1b[<0;10;5M");
        assert_eq!(
            ev,
            vec![Event::Mouse(MouseEvent {
                kind: MouseEventKind::Press(MouseButton::Left),
                x: 9,
                y: 4,
                unit: CoordUnit::Cell,
                modifiers: Modifiers::NONE
            })]
        );
        let mut p = Parser::new();
        p.set_pixel_mouse(true);
        let ev = p.feed(b"\x1b[<32;300;119M\x1b[<0;300;119m\x1b[<35;1;1M");
        assert_eq!(ev.len(), 3);
        assert!(matches!(
            ev[0],
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Drag(MouseButton::Left),
                x: 300,
                y: 119,
                unit: CoordUnit::Pixel,
                ..
            })
        ));
        assert!(matches!(ev[1], Event::Mouse(MouseEvent { kind: MouseEventKind::Release(MouseButton::Left), .. })));
        assert!(matches!(ev[2], Event::Mouse(MouseEvent { kind: MouseEventKind::Move, .. })));
    }

    #[test]
    fn sgr_scroll_with_ctrl() {
        let ev = parse(b"\x1b[<80;3;3M");
        assert!(matches!(
            ev[0],
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Scroll(ScrollDirection::Up),
                modifiers: Modifiers::CTRL,
                ..
            })
        ));
        let ev = parse(b"\x1b[<67;3;3M");
        assert!(matches!(ev[0], Event::Mouse(MouseEvent { kind: MouseEventKind::Scroll(ScrollDirection::Right), .. })));
    }

    #[test]
    fn sgr_extra_buttons_are_8_to_11() {
        // xterm sends buttons 8 to 11 (back, forward, ...) as 128 + 0..3.
        for (code, button) in [(128, 8), (129, 9), (130, 10), (131, 11)] {
            let ev = parse(format!("\x1b[<{code};3;3M").as_bytes());
            assert_eq!(
                ev[0],
                Event::Mouse(MouseEvent {
                    kind: MouseEventKind::Press(MouseButton::Other(button)),
                    x: 2,
                    y: 2,
                    unit: CoordUnit::Cell,
                    modifiers: Modifiers::NONE,
                }),
                "code {code}"
            );
        }
    }

    #[test]
    fn bracketed_paste_split() {
        let mut p = Parser::new();
        assert!(p.feed(b"\x1b[200~hello \x1b[A wor").is_empty());
        assert!(p.feed(b"ld\x1b[20").is_empty());
        let ev = p.feed(b"1~x");
        assert_eq!(ev[0], Event::Paste("hello \x1b[A world".into()));
        assert!(matches!(&ev[1], Event::Key(KeyEvent { code: KeyCode::Char('x'), .. })));
    }

    #[test]
    fn focus_and_resize() {
        assert_eq!(parse(b"\x1b[I\x1b[O"), vec![Event::Focus(true), Event::Focus(false)]);
        assert_eq!(
            parse(b"\x1b[48;40;120;800;1200t"),
            vec![Event::Resize(ResizeReport { rows: 40, cols: 120, height_px: 800, width_px: 1200 })]
        );
    }

    #[test]
    fn graphics_replies() {
        let ev = parse(b"\x1b_Gi=31;OK\x1b\\\x1b_Gi=2,p=7;ENOENT:image not found\x1b\\");
        let Event::Graphics(ok) = &ev[0] else { panic!() };
        assert!(ok.is_ok());
        assert_eq!(ok.image_id, Some(31));
        let Event::Graphics(err) = &ev[1] else { panic!() };
        assert_eq!(err.error_code(), Some("ENOENT"));
        assert_eq!(err.placement_id, Some(7));
    }

    #[test]
    fn graphics_reply_split() {
        let mut p = Parser::new();
        assert!(p.feed(b"\x1b_Gi=1;O").is_empty());
        assert!(p.feed(b"K\x1b").is_empty());
        assert!(matches!(&p.feed(b"\\")[0], Event::Graphics(r) if r.is_ok()));
    }

    #[test]
    fn query_replies() {
        assert_eq!(parse(b"\x1b[?62;22;52c"), vec![Event::Reply(Reply::DeviceAttributes(vec![62, 22, 52]))]);
        assert_eq!(parse(b"\x1b[?2026;2$y"), vec![Event::Reply(Reply::Mode { mode: 2026, state: ModeState::Reset })]);
        assert_eq!(parse(b"\x1bP>|kitty(0.49.2)\x1b\\"), vec![Event::Reply(Reply::Version("kitty(0.49.2)".into()))]);
        assert_eq!(parse(b"\x1b[6;20;10t"), vec![Event::Reply(Reply::CellSize { width: 10, height: 20 })]);
        assert_eq!(parse(b"\x1b[4;800;1200t"), vec![Event::Reply(Reply::WindowSize { width: 1200, height: 800 })]);
        assert_eq!(parse(b"\x1b[?31u"), vec![Event::Reply(Reply::KeyboardFlags(31))]);
        assert_eq!(
            parse(b"\x1b]11;rgb:0000/0000/0000\x07"),
            vec![Event::Reply(Reply::Osc(b"11;rgb:0000/0000/0000".to_vec()))]
        );
    }

    #[test]
    fn cursor_report_is_not_f3() {
        assert!(matches!(&parse(b"\x1b[12;40R")[0], Event::Unknown(_)));
        assert_eq!(parse(b"\x1b[1;2R"), vec![k(KeyCode::F(3), Modifiers::SHIFT)]);
    }

    #[test]
    fn incomplete_csi_waits() {
        let mut p = Parser::new();
        assert!(p.feed(b"\x1b[<0;1").is_empty());
        assert_eq!(p.feed(b";1M").len(), 1);
    }
}
