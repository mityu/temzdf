//! Window input as the session's events: keys as the kitty keyboard
//! protocol reports them (so mappings read the same as in a terminal),
//! pointer events in pixels, scrolling with its amount and source, focus,
//! and an input method's text. Also the clipboard (for `Copy` and `Paste`
//! effects) and where the input method's window goes.
//!
//! winit's events cannot be built in tests, so keys go through [`KeyInput`]
//! and the pure [`translate`].

use temzdf_protocol::ClientMsg;
use termpix_core::geom::Geometry;
use termpix_core::input::{
    CoordUnit, KeyCode, KeyEvent, KeyEventKind, ModifierKey, Modifiers, MouseButton, MouseEvent, MouseEventKind,
    ScrollPhase, ScrollUnit,
};
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::{ButtonSource, ElementState, Ime, MouseScrollDelta, ScrollSource, WindowEvent};
use winit::keyboard::{Key, KeyLocation, ModifiersState, NamedKey};
use winit::window::{ImeCapabilities, ImeEnableRequest, ImeRequest, ImeRequestData, Window};

/// Let the input method compose into the window, its candidates placed at
/// the cursor area (set as the text cursor moves).
pub fn enable_ime(window: &dyn Window) {
    let mut data = ImeRequestData::default();
    data.cursor_area = Some((PhysicalPosition::new(0, 0).into(), PhysicalSize::new(1, 1).into()));
    if let Some(request) = ImeEnableRequest::new(ImeCapabilities::new().with_cursor_area(), data) {
        let _ = window.request_ime_update(ImeRequest::Enable(request));
    }
}

/// A key as the window reported it (what [`translate`] needs).
#[derive(Clone, Debug, PartialEq)]
pub struct KeyInput {
    pub key: Logical,
    /// The text it types, if any.
    pub text: Option<String>,
    pub mods: Modifiers,
    pub pressed: bool,
    pub repeat: bool,
    pub left_side: bool,
    /// The key in the US layout (from the physical key), for Ctrl+<a
    /// letter of another layout>.
    pub base: Option<char>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Logical {
    /// What the key gives with every modifier but Ctrl (winit's rule).
    Char(String),
    Named(NamedKey),
    /// A dead key or one with no meaning here.
    Other,
}

/// A key as the session takes it: the character (with Shift folded in, as
/// typed) or the named key, and the modifiers; with Ctrl, Alt or Super a
/// letter is its base letter and Shift stays a modifier (`<C-S-v>`).
pub fn translate(k: &KeyInput) -> Option<KeyEvent> {
    let kind = match (k.pressed, k.repeat) {
        (false, _) => KeyEventKind::Release,
        (true, true) => KeyEventKind::Repeat,
        (true, false) => KeyEventKind::Press,
    };
    let chorded =
        k.mods.contains(Modifiers::CTRL) || k.mods.contains(Modifiers::ALT) || k.mods.contains(Modifiers::SUPER);
    let code = match &k.key {
        Logical::Char(s) => {
            let c = s.chars().next()?;
            if chorded && c.is_alphabetic() {
                KeyCode::Char(c.to_lowercase().next().unwrap_or(c))
            } else {
                KeyCode::Char(c)
            }
        }
        Logical::Named(n) => named(*n, k.left_side)?,
        Logical::Other => return None,
    };
    let text = match (&k.key, chorded) {
        (Logical::Char(_), false) => k.text.clone().filter(|t| !t.chars().any(char::is_control)),
        _ => None,
    };
    Some(KeyEvent { code, modifiers: k.mods, kind, text, base: k.base })
}

fn named(n: NamedKey, left: bool) -> Option<KeyCode> {
    let side = |l: ModifierKey, r: ModifierKey| KeyCode::Modifier(if left { l } else { r });
    Some(match n {
        NamedKey::Enter => KeyCode::Enter,
        NamedKey::Tab => KeyCode::Tab,
        NamedKey::Backspace => KeyCode::Backspace,
        NamedKey::Escape => KeyCode::Escape,
        NamedKey::ArrowUp => KeyCode::Up,
        NamedKey::ArrowDown => KeyCode::Down,
        NamedKey::ArrowLeft => KeyCode::Left,
        NamedKey::ArrowRight => KeyCode::Right,
        NamedKey::Home => KeyCode::Home,
        NamedKey::End => KeyCode::End,
        NamedKey::PageUp => KeyCode::PageUp,
        NamedKey::PageDown => KeyCode::PageDown,
        NamedKey::Insert => KeyCode::Insert,
        NamedKey::Delete => KeyCode::Delete,
        NamedKey::CapsLock => KeyCode::CapsLock,
        NamedKey::ScrollLock => KeyCode::ScrollLock,
        NamedKey::NumLock => KeyCode::NumLock,
        NamedKey::PrintScreen => KeyCode::PrintScreen,
        NamedKey::Pause => KeyCode::Pause,
        NamedKey::ContextMenu => KeyCode::Menu,
        NamedKey::Shift => side(ModifierKey::LeftShift, ModifierKey::RightShift),
        NamedKey::Control => side(ModifierKey::LeftCtrl, ModifierKey::RightCtrl),
        NamedKey::Alt => side(ModifierKey::LeftAlt, ModifierKey::RightAlt),
        NamedKey::Meta => side(ModifierKey::LeftSuper, ModifierKey::RightSuper),
        NamedKey::F1 => KeyCode::F(1),
        NamedKey::F2 => KeyCode::F(2),
        NamedKey::F3 => KeyCode::F(3),
        NamedKey::F4 => KeyCode::F(4),
        NamedKey::F5 => KeyCode::F(5),
        NamedKey::F6 => KeyCode::F(6),
        NamedKey::F7 => KeyCode::F(7),
        NamedKey::F8 => KeyCode::F(8),
        NamedKey::F9 => KeyCode::F(9),
        NamedKey::F10 => KeyCode::F(10),
        NamedKey::F11 => KeyCode::F(11),
        NamedKey::F12 => KeyCode::F(12),
        _ => return None,
    })
}

/// The US-layout character of a physical key (letters, digits and the
/// punctuation keys), whatever the layout in use.
fn base_key(key: &winit::keyboard::PhysicalKey) -> Option<char> {
    use winit::keyboard::{KeyCode as K, PhysicalKey};
    let PhysicalKey::Code(code) = key else { return None };
    const LETTERS: [K; 26] = [
        K::KeyA,
        K::KeyB,
        K::KeyC,
        K::KeyD,
        K::KeyE,
        K::KeyF,
        K::KeyG,
        K::KeyH,
        K::KeyI,
        K::KeyJ,
        K::KeyK,
        K::KeyL,
        K::KeyM,
        K::KeyN,
        K::KeyO,
        K::KeyP,
        K::KeyQ,
        K::KeyR,
        K::KeyS,
        K::KeyT,
        K::KeyU,
        K::KeyV,
        K::KeyW,
        K::KeyX,
        K::KeyY,
        K::KeyZ,
    ];
    const DIGITS: [K; 10] =
        [K::Digit0, K::Digit1, K::Digit2, K::Digit3, K::Digit4, K::Digit5, K::Digit6, K::Digit7, K::Digit8, K::Digit9];
    if let Some(i) = LETTERS.iter().position(|k| k == code) {
        return Some((b'a' + i as u8) as char);
    }
    if let Some(i) = DIGITS.iter().position(|k| k == code) {
        return Some((b'0' + i as u8) as char);
    }
    Some(match code {
        K::Minus => '-',
        K::Equal => '=',
        K::BracketLeft => '[',
        K::BracketRight => ']',
        K::Backslash => '\\',
        K::Semicolon => ';',
        K::Quote => '\'',
        K::Backquote => '`',
        K::Comma => ',',
        K::Period => '.',
        K::Slash => '/',
        _ => return None,
    })
}

fn modifiers(s: ModifiersState) -> Modifiers {
    let mut m = Modifiers::NONE;
    for (w, t) in [
        (ModifiersState::SHIFT, Modifiers::SHIFT),
        (ModifiersState::CONTROL, Modifiers::CTRL),
        (ModifiersState::ALT, Modifiers::ALT),
        (ModifiersState::META, Modifiers::SUPER),
    ] {
        if s.contains(w) {
            m |= t;
        }
    }
    m
}

/// A wheel or trackpad amount as `ScrollBy`: positive is down and right.
/// winit's positive y scrolls up (the content moves down), as its x moves
/// the view left.
pub fn scroll(delta: MouseScrollDelta, source: ScrollSource) -> MouseEventKind {
    let (dx, dy, unit) = match delta {
        MouseScrollDelta::LineDelta(x, y) => (-x, -y, ScrollUnit::Line),
        MouseScrollDelta::PixelDelta(p) => (-p.x as f32, -p.y as f32, ScrollUnit::Pixel),
        _ => (0.0, 0.0, ScrollUnit::Line),
    };
    let phase = match (source, unit) {
        (ScrollSource::Finger, _) => ScrollPhase::Touch,
        (ScrollSource::Momentum, _) => ScrollPhase::Momentum,
        (ScrollSource::Wheel, _) | (_, ScrollUnit::Line) => ScrollPhase::Wheel,
        _ => ScrollPhase::Touch,
    };
    let hundredths = |v: f32| (v * 100.0).round() as i32;
    MouseEventKind::ScrollBy { dx: hundredths(dx), dy: hundredths(dy), unit, phase }
}

/// The input state of the window.
pub struct Input {
    mods: Modifiers,
    left_side: bool,
    /// The buttons held, in the order pressed (drags carry the last).
    held: Vec<MouseButton>,
    pointer: (i32, i32),
    /// Text the input method is composing, and where the cursor was (its
    /// cell, and the cell size: the candidate window goes there).
    pub preedit: String,
    ime_cell: Option<((u16, u16), termpix_core::geom::CellSize)>,
    clipboard: Option<arboard::Clipboard>,
}

impl Default for Input {
    fn default() -> Self {
        Self::new()
    }
}

impl Input {
    pub fn new() -> Self {
        Self { clipboard: arboard::Clipboard::new().ok(), ..Self::without_clipboard() }
    }

    /// No clipboard (tests: opening one connects to the display).
    fn without_clipboard() -> Self {
        Self {
            mods: Modifiers::NONE,
            left_side: true,
            held: Vec::new(),
            pointer: (0, 0),
            preedit: String::new(),
            ime_cell: None,
            clipboard: None,
        }
    }

    pub fn clipboard_set(&mut self, text: &str, primary: bool) -> Result<(), String> {
        let c = self.clipboard.as_mut().ok_or("no clipboard")?;
        #[cfg(target_os = "linux")]
        if primary {
            use arboard::SetExtLinux;
            return c.set().clipboard(arboard::LinuxClipboardKind::Primary).text(text).map_err(|e| e.to_string());
        }
        let _ = primary;
        c.set_text(text).map_err(|e| e.to_string())
    }

    pub fn clipboard_get(&mut self, primary: bool) -> Result<String, String> {
        let c = self.clipboard.as_mut().ok_or("no clipboard")?;
        #[cfg(target_os = "linux")]
        if primary {
            use arboard::GetExtLinux;
            return c.get().clipboard(arboard::LinuxClipboardKind::Primary).text().map_err(|e| e.to_string());
        }
        let _ = primary;
        c.get_text().map_err(|e| e.to_string())
    }

    /// Put the input method's window at the text cursor's cell.
    pub fn set_cursor_cell(&mut self, cell: Option<(u16, u16)>, g: Geometry, window: &dyn Window) {
        if !self.ime_moved(cell, g) {
            return;
        }
        if let Some((col, row)) = cell {
            let (w, h) = (g.cell.width, g.cell.height);
            let mut data = ImeRequestData::default();
            data.cursor_area =
                Some((PhysicalPosition::new(col as u32 * w, row as u32 * h).into(), PhysicalSize::new(w, h).into()));
            let _ = window.request_ime_update(ImeRequest::Update(data));
        }
    }

    /// Whether the cursor's cell moved on the window: another cell, or the
    /// same at another cell size (font size, scale).
    fn ime_moved(&mut self, cell: Option<(u16, u16)>, g: Geometry) -> bool {
        let at = cell.map(|c| (c, g.cell));
        if at == self.ime_cell {
            return false;
        }
        self.ime_cell = at;
        true
    }

    /// A button went down or up.
    fn button(&mut self, b: MouseButton, pressed: bool) -> MouseEventKind {
        self.held.retain(|h| *h != b);
        if pressed {
            self.held.push(b);
            MouseEventKind::Press(b)
        } else {
            MouseEventKind::Release(b)
        }
    }

    /// The pointer moved: a drag while a button is held.
    fn moved(&self) -> MouseEventKind {
        self.held.last().map_or(MouseEventKind::Move, |b| MouseEventKind::Drag(*b))
    }

    fn mouse(&self, kind: MouseEventKind) -> ClientMsg {
        let (x, y) = self.pointer;
        ClientMsg::Mouse(MouseEvent { kind, x, y, unit: CoordUnit::Pixel, modifiers: self.mods })
    }

    /// What a window event means for the session.
    /// An input method event: what goes to the session, and whether the
    /// window must be drawn again (the composed text changed).
    pub fn ime(&mut self, ime: &Ime) -> (Vec<ClientMsg>, bool) {
        match ime {
            Ime::Preedit(text, _) => {
                self.preedit = text.clone();
                (Vec::new(), true)
            }
            Ime::Commit(text) => {
                self.preedit.clear();
                (vec![ClientMsg::Text(text.clone())], true)
            }
            // Turned off (a focus change, macOS switching input sources)
            // mid-composition: nothing is being composed any more.
            Ime::Disabled => {
                let had = !self.preedit.is_empty();
                self.preedit.clear();
                (Vec::new(), had)
            }
            _ => (Vec::new(), false),
        }
    }

    pub fn window_event(&mut self, event: &WindowEvent, _g: Geometry, window: &dyn Window) -> Vec<ClientMsg> {
        match event {
            WindowEvent::ModifiersChanged(m) => {
                self.mods = modifiers(m.state());
                Vec::new()
            }
            WindowEvent::KeyboardInput { event, .. } => {
                // Keys the input method takes do not come here.
                self.left_side = event.location != KeyLocation::Right;
                let key = match &event.logical_key {
                    Key::Character(s) => Logical::Char(s.to_string()),
                    Key::Named(n) => Logical::Named(*n),
                    _ => Logical::Other,
                };
                let k = KeyInput {
                    key,
                    text: event.text.as_ref().map(|t| t.to_string()),
                    mods: self.mods,
                    pressed: event.state == ElementState::Pressed,
                    repeat: event.repeat,
                    left_side: self.left_side,
                    base: base_key(&event.physical_key),
                };
                translate(&k).map(ClientMsg::Key).into_iter().collect()
            }
            WindowEvent::Ime(ime) => {
                let (msgs, redraw) = self.ime(ime);
                if redraw {
                    window.request_redraw();
                }
                msgs
            }
            WindowEvent::Focused(gained) => {
                // A composition does not survive the focus going (Wayland
                // ends it without a word); its text must not stay drawn.
                if !gained && !self.preedit.is_empty() {
                    self.preedit.clear();
                    window.request_redraw();
                }
                vec![ClientMsg::Focus(*gained)]
            }
            WindowEvent::PointerMoved { position, .. } => {
                self.pointer = (position.x as i32, position.y as i32);
                vec![self.mouse(self.moved())]
            }
            WindowEvent::PointerLeft { .. } => vec![self.mouse(MouseEventKind::Leave)],
            WindowEvent::PointerButton { state, button: ButtonSource::Mouse(b), position, .. } => {
                self.pointer = (position.x as i32, position.y as i32);
                use winit::event::MouseButton as W;
                let b = match b {
                    W::Left => MouseButton::Left,
                    W::Middle => MouseButton::Middle,
                    W::Right => MouseButton::Right,
                    W::Back => MouseButton::Other(8),
                    W::Forward => MouseButton::Other(9),
                    _ => return Vec::new(),
                };
                let kind = self.button(b, *state == ElementState::Pressed);
                vec![self.mouse(kind)]
            }
            WindowEvent::MouseWheel { delta, source, .. } => vec![self.mouse(scroll(*delta, *source))],
            _ => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(key: Logical, text: Option<&str>, mods: Modifiers) -> Option<KeyEvent> {
        translate(&KeyInput {
            key,
            text: text.map(Into::into),
            mods,
            pressed: true,
            repeat: false,
            left_side: true,
            base: None,
        })
    }

    fn ch(s: &str) -> Logical {
        Logical::Char(s.into())
    }

    #[test]
    fn keys_read_as_the_kitty_protocol_reports_them() {
        let k = key(ch("a"), Some("a"), Modifiers::NONE).unwrap();
        assert_eq!((k.code, k.modifiers, k.text.as_deref()), (KeyCode::Char('a'), Modifiers::NONE, Some("a")));
        // Shift: the character typed.
        let k = key(ch("G"), Some("G"), Modifiers::SHIFT).unwrap();
        assert_eq!((k.code, k.text.as_deref()), (KeyCode::Char('G'), Some("G")));
        // Ctrl (Shift too): the base letter, no text.
        let k = key(ch("V"), None, Modifiers::CTRL | Modifiers::SHIFT).unwrap();
        assert_eq!((k.code, k.modifiers, k.text), (KeyCode::Char('v'), Modifiers::CTRL | Modifiers::SHIFT, None));
        let k = key(ch("ä"), Some("ä"), Modifiers::NONE).unwrap();
        assert_eq!(k.code, KeyCode::Char('ä'), "beyond ASCII as typed");
        assert_eq!(key(Logical::Named(NamedKey::Enter), Some("\r"), Modifiers::NONE).unwrap().code, KeyCode::Enter);
        let k = key(Logical::Named(NamedKey::Enter), Some("\r"), Modifiers::NONE).unwrap();
        assert_eq!(k.text, None, "control characters are no text");
        assert_eq!(key(Logical::Named(NamedKey::F5), None, Modifiers::ALT).unwrap().code, KeyCode::F(5));
        assert_eq!(key(ch(" "), Some(" "), Modifiers::CTRL).unwrap().code, KeyCode::Char(' '));
        assert_eq!(
            key(Logical::Named(NamedKey::Control), None, Modifiers::CTRL).unwrap().code,
            KeyCode::Modifier(ModifierKey::LeftCtrl)
        );
        assert!(key(Logical::Other, None, Modifiers::NONE).is_none(), "dead keys wait for the input method");
        let up = translate(&KeyInput {
            key: ch("a"),
            text: None,
            mods: Modifiers::NONE,
            pressed: false,
            repeat: false,
            left_side: true,
            base: None,
        });
        assert_eq!(up.unwrap().kind, KeyEventKind::Release);
    }

    #[test]
    fn drags_carry_a_button_still_held() {
        let mut input = Input::without_clipboard();
        assert_eq!(input.moved(), MouseEventKind::Move);
        input.button(MouseButton::Left, true);
        input.button(MouseButton::Right, true);
        assert_eq!(input.moved(), MouseEventKind::Drag(MouseButton::Right));
        // The first one let go: the other is still held.
        assert_eq!(input.button(MouseButton::Left, false), MouseEventKind::Release(MouseButton::Left));
        assert_eq!(input.moved(), MouseEventKind::Drag(MouseButton::Right));
        input.button(MouseButton::Right, false);
        assert_eq!(input.moved(), MouseEventKind::Move);
    }

    #[test]
    fn the_candidate_window_follows_the_cell_size() {
        use termpix_core::geom::CellSize;
        let mut input = Input::without_clipboard();
        let g = Geometry { cols: 80, rows: 24, cell: CellSize { width: 10, height: 20 } };
        assert!(input.ime_moved(Some((3, 4)), g));
        assert!(!input.ime_moved(Some((3, 4)), g), "nothing moved");
        let larger = Geometry { cell: CellSize { width: 12, height: 24 }, ..g };
        assert!(input.ime_moved(Some((3, 4)), larger), "the same cell, larger");
    }

    #[test]
    fn the_composition_goes_when_the_input_method_turns_off() {
        let mut input = Input::without_clipboard();
        let (msgs, redraw) = input.ime(&Ime::Preedit("にほ".into(), None));
        assert!(msgs.is_empty() && redraw);
        assert_eq!(input.preedit, "にほ");
        let (msgs, redraw) = input.ime(&Ime::Disabled);
        assert!(msgs.is_empty() && redraw, "drawn again without it");
        assert!(input.preedit.is_empty());
        input.ime(&Ime::Preedit("に".into(), None));
        assert_eq!(input.ime(&Ime::Commit("日".into())).0, [ClientMsg::Text("日".into())]);
        assert!(input.preedit.is_empty());
    }

    #[test]
    fn physical_keys_give_the_base_layout_key() {
        use winit::keyboard::{KeyCode as K, PhysicalKey};
        let base = |k| base_key(&PhysicalKey::Code(k));
        assert_eq!(
            (base(K::KeyC), base(K::Digit7), base(K::Slash), base(K::Enter)),
            (Some('c'), Some('7'), Some('/'), None)
        );
        let k = KeyInput { base: Some('c'), ..ru_ctrl() };
        assert_eq!(translate(&k).unwrap().base, Some('c'), "carried to the session");
    }

    fn ru_ctrl() -> KeyInput {
        KeyInput {
            key: Logical::Char("с".into()),
            text: None,
            mods: Modifiers::CTRL,
            pressed: true,
            repeat: false,
            left_side: true,
            base: None,
        }
    }

    #[test]
    fn scrolling_amounts_point_down_and_right() {
        let s = scroll(MouseScrollDelta::LineDelta(0.0, -1.0), ScrollSource::Wheel);
        assert_eq!(s, MouseEventKind::ScrollBy { dx: 0, dy: 100, unit: ScrollUnit::Line, phase: ScrollPhase::Wheel });
        let s = scroll(MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, 12.5)), ScrollSource::Finger);
        assert_eq!(
            s,
            MouseEventKind::ScrollBy { dx: 0, dy: -1250, unit: ScrollUnit::Pixel, phase: ScrollPhase::Touch }
        );
        let s = scroll(MouseScrollDelta::PixelDelta(PhysicalPosition::new(-3.0, 0.0)), ScrollSource::Momentum);
        assert_eq!(
            s,
            MouseEventKind::ScrollBy { dx: 300, dy: 0, unit: ScrollUnit::Pixel, phase: ScrollPhase::Momentum }
        );
    }
}
