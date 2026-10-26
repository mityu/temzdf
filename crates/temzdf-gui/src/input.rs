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
    Some(KeyEvent { code, modifiers: k.mods, kind, text })
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
    /// The button held (drags carry it).
    held: Option<MouseButton>,
    pointer: (i32, i32),
    /// Text the input method is composing, and where the cursor was.
    pub preedit: String,
    ime_cell: Option<(u16, u16)>,
    clipboard: Option<arboard::Clipboard>,
}

impl Default for Input {
    fn default() -> Self {
        Self::new()
    }
}

impl Input {
    pub fn new() -> Self {
        Self {
            mods: Modifiers::NONE,
            left_side: true,
            held: None,
            pointer: (0, 0),
            preedit: String::new(),
            ime_cell: None,
            clipboard: arboard::Clipboard::new().ok(),
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
        if cell == self.ime_cell {
            return;
        }
        self.ime_cell = cell;
        if let Some((col, row)) = cell {
            let (w, h) = (g.cell.width, g.cell.height);
            let mut data = ImeRequestData::default();
            data.cursor_area =
                Some((PhysicalPosition::new(col as u32 * w, row as u32 * h).into(), PhysicalSize::new(w, h).into()));
            let _ = window.request_ime_update(ImeRequest::Update(data));
        }
    }

    fn mouse(&self, kind: MouseEventKind) -> ClientMsg {
        let (x, y) = self.pointer;
        ClientMsg::Mouse(MouseEvent { kind, x, y, unit: CoordUnit::Pixel, modifiers: self.mods })
    }

    /// What a window event means for the session.
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
                };
                translate(&k).map(ClientMsg::Key).into_iter().collect()
            }
            WindowEvent::Ime(Ime::Preedit(text, _)) => {
                self.preedit = text.clone();
                window.request_redraw();
                Vec::new()
            }
            WindowEvent::Ime(Ime::Commit(text)) => {
                self.preedit.clear();
                window.request_redraw();
                vec![ClientMsg::Text(text.clone())]
            }
            WindowEvent::Focused(gained) => vec![ClientMsg::Focus(*gained)],
            WindowEvent::PointerMoved { position, .. } => {
                self.pointer = (position.x as i32, position.y as i32);
                let kind = match self.held {
                    Some(b) => MouseEventKind::Drag(b),
                    None => MouseEventKind::Move,
                };
                vec![self.mouse(kind)]
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
                let kind = if *state == ElementState::Pressed {
                    self.held = Some(b);
                    MouseEventKind::Press(b)
                } else {
                    self.held = None;
                    MouseEventKind::Release(b)
                };
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
        translate(&KeyInput { key, text: text.map(Into::into), mods, pressed: true, repeat: false, left_side: true })
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
        });
        assert_eq!(up.unwrap().kind, KeyEventKind::Release);
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
