//! Input events shared by every frontend: keys and mouse, independent of how
//! they were read (terminal escape sequences in `termpix::input`, or a window
//! system).

use std::fmt;

use crate::geom::CellSize;

/// Keyboard modifiers, using the kitty keyboard protocol bit layout.
#[derive(Clone, Copy, Default, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Modifiers(u8);

impl Modifiers {
    pub const NONE: Self = Self(0);
    pub const SHIFT: Self = Self(1);
    pub const ALT: Self = Self(2);
    pub const CTRL: Self = Self(4);
    pub const SUPER: Self = Self(8);
    pub const HYPER: Self = Self(16);
    pub const META: Self = Self(32);
    pub const CAPS_LOCK: Self = Self(64);
    pub const NUM_LOCK: Self = Self(128);

    pub const fn bits(self) -> u8 {
        self.0
    }

    pub const fn from_bits(bits: u8) -> Self {
        Self(bits)
    }

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// The modifiers that matter for shortcuts (lock keys stripped).
    pub const fn without_locks(self) -> Self {
        Self(self.0 & !(Self::CAPS_LOCK.0 | Self::NUM_LOCK.0))
    }
}

impl std::ops::BitOr for Modifiers {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl std::ops::BitOrAssign for Modifiers {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

impl fmt::Debug for Modifiers {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        const NAMES: [&str; 8] = ["Shift", "Alt", "Ctrl", "Super", "Hyper", "Meta", "CapsLock", "NumLock"];
        let mut first = true;
        write!(f, "Modifiers(")?;
        for (i, name) in NAMES.iter().enumerate() {
            if self.0 & (1 << i) != 0 {
                if !first {
                    write!(f, "|")?;
                }
                write!(f, "{name}")?;
                first = false;
            }
        }
        write!(f, ")")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ModifierKey {
    LeftShift,
    LeftCtrl,
    LeftAlt,
    LeftSuper,
    LeftHyper,
    LeftMeta,
    RightShift,
    RightCtrl,
    RightAlt,
    RightSuper,
    RightHyper,
    RightMeta,
    IsoLevel3Shift,
    IsoLevel5Shift,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum KeyCode {
    Char(char),
    Enter,
    Tab,
    Backspace,
    Escape,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
    /// Keypad centre key (`CSI E`).
    Begin,
    F(u8),
    CapsLock,
    ScrollLock,
    NumLock,
    PrintScreen,
    Pause,
    Menu,
    Modifier(ModifierKey),
    /// A kitty functional key we do not name (keypad, media keys, ...): the raw code.
    Other(u32),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum KeyEventKind {
    #[default]
    Press,
    Repeat,
    Release,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct KeyEvent {
    pub code: KeyCode,
    pub modifiers: Modifiers,
    pub kind: KeyEventKind,
    /// Text the key produces (kitty "associated text"), when the terminal reports it.
    pub text: Option<String>,
    /// The key in the base (US) layout, when known (kitty's alternate keys,
    /// a GUI's physical key): `c` for Ctrl+С on a Russian layout, so a
    /// terminal program still gets Ctrl+C.
    #[cfg_attr(feature = "serde", serde(default))]
    pub base: Option<char>,
}

impl KeyEvent {
    pub fn new(code: KeyCode, modifiers: Modifiers) -> Self {
        Self { code, modifiers, kind: KeyEventKind::Press, text: None, base: None }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    /// Buttons 8-11 ("back", "forward", ...): the SGR button number.
    Other(u8),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ScrollDirection {
    Up,
    Down,
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum MouseEventKind {
    Press(MouseButton),
    Release(MouseButton),
    /// Motion with a button held.
    Drag(MouseButton),
    /// Motion with no button held (needs mode 1003).
    Move,
    Scroll(ScrollDirection),
    /// Scrolling by an amount (GUI clients: trackpads, high-resolution
    /// wheels), in hundredths of `unit`; positive is down and right, as
    /// `Scroll(Down)` and `Scroll(Right)`.
    ScrollBy {
        dx: i32,
        dy: i32,
        unit: ScrollUnit,
        phase: ScrollPhase,
    },
    /// The pointer left the window (kitty extension to mode 1016).
    Leave,
    /// Two fingers pinching on a trackpad (GUI clients, where the window
    /// system reports it): the magnification times `1 + delta / 10000`
    /// (in when positive), about the pointer.
    Pinch {
        delta: i32,
        phase: PinchPhase,
    },
}

/// What a [`MouseEventKind::ScrollBy`] amount counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ScrollUnit {
    /// Pixels of the screen.
    Pixel,
    /// Wheel notches (a fraction from high-resolution wheels).
    Line,
}

/// Where a [`MouseEventKind::ScrollBy`] comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ScrollPhase {
    /// A wheel: views may animate it as their own wheel steps.
    Wheel,
    /// Fingers on a trackpad: followed exactly.
    Touch,
    /// The system's inertia after the fingers left: followed exactly, with
    /// no inertia of the view's own on top.
    Momentum,
}

/// Where a [`MouseEventKind::Pinch`] is in its gesture.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum PinchPhase {
    /// The fingers started pinching.
    Begin,
    /// They moved.
    Change,
    /// They left (or the system cancelled the gesture).
    End,
}

/// Rows a wheel notch scrolls in panes of text (terminals, text panes).
pub const WHEEL_ROWS: u32 = 3;

impl MouseEventKind {
    /// Any kind of scrolling.
    pub fn is_scroll(&self) -> bool {
        matches!(self, Self::Scroll(_) | Self::ScrollBy { .. })
    }

    /// Vertical scrolling in rows of `cell_height` pixels: a notch is
    /// [`WHEEL_ROWS`], pixels are the cell height's share (a fraction).
    /// `None` for other events and sideways scrolling.
    pub fn wheel_rows(&self, cell_height: u32) -> Option<f32> {
        match *self {
            Self::Scroll(ScrollDirection::Up) => Some(-(WHEEL_ROWS as f32)),
            Self::Scroll(ScrollDirection::Down) => Some(WHEEL_ROWS as f32),
            Self::ScrollBy { dy, unit: ScrollUnit::Line, .. } => Some(dy as f32 / 100.0 * WHEEL_ROWS as f32),
            Self::ScrollBy { dy, unit: ScrollUnit::Pixel, .. } => Some(dy as f32 / 100.0 / cell_height.max(1) as f32),
            _ => None,
        }
    }
}

/// Fractional scroll amounts adding up until they make whole steps (rows,
/// or notches of several rows), as a touchpad's small deltas do.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct WheelAccumulator(f32);

impl WheelAccumulator {
    pub fn add(&mut self, rows: f32) {
        self.0 += rows;
    }

    /// The whole steps of `per` rows gathered so far (negative upwards),
    /// taken out; the rest stays.
    pub fn take(&mut self, per: f32) -> i32 {
        let steps = (self.0 / per).trunc();
        self.0 -= steps * per;
        steps as i32
    }
}

/// Unit of mouse coordinates, decided by whether mode 1016 is active.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum CoordUnit {
    #[default]
    Cell,
    Pixel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct MouseEvent {
    pub kind: MouseEventKind,
    /// Zero-based column (cells) or x (pixels), see `unit`.
    pub x: i32,
    /// Zero-based row (cells) or y (pixels), see `unit`.
    pub y: i32,
    pub unit: CoordUnit,
    pub modifiers: Modifiers,
}

impl MouseEvent {
    /// The position in pixels; an event in cells is at its cell's centre.
    pub fn pixel_pos(&self, cell: CellSize) -> (f32, f32) {
        match self.unit {
            CoordUnit::Pixel => (self.x as f32, self.y as f32),
            CoordUnit::Cell => ((self.x as f32 + 0.5) * cell.width as f32, (self.y as f32 + 0.5) * cell.height as f32),
        }
    }

    /// The cell (column, row) under the event.
    pub fn cell_pos(&self, cell: CellSize) -> (i32, i32) {
        match self.unit {
            CoordUnit::Pixel => {
                (self.x.div_euclid(cell.width.max(1) as i32), self.y.div_euclid(cell.height.max(1) as i32))
            }
            CoordUnit::Cell => (self.x, self.y),
        }
    }

    /// The event relative to the cell (`col`, `row`) as the origin, in its
    /// own unit.
    pub fn offset(&self, col: i32, row: i32, cell: CellSize) -> Self {
        let (dx, dy) = match self.unit {
            CoordUnit::Pixel => (col * cell.width as i32, row * cell.height as i32),
            CoordUnit::Cell => (col, row),
        };
        Self { x: self.x - dx, y: self.y - dy, ..*self }
    }

    /// The event in pixels; an event in cells is at its cell's top left.
    pub fn in_pixels(&self, cell: CellSize) -> Self {
        match self.unit {
            CoordUnit::Pixel => *self,
            CoordUnit::Cell => {
                Self { x: self.x * cell.width as i32, y: self.y * cell.height as i32, unit: CoordUnit::Pixel, ..*self }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CELL: CellSize = CellSize { width: 10, height: 20 };

    fn at(x: i32, y: i32, unit: CoordUnit) -> MouseEvent {
        MouseEvent { kind: MouseEventKind::Move, x, y, unit, modifiers: Modifiers::NONE }
    }

    #[test]
    fn positions_in_cells_and_pixels() {
        assert_eq!(at(2, 1, CoordUnit::Cell).pixel_pos(CELL), (25.0, 30.0));
        assert_eq!(at(25, 30, CoordUnit::Pixel).pixel_pos(CELL), (25.0, 30.0));
        assert_eq!(at(25, 30, CoordUnit::Pixel).cell_pos(CELL), (2, 1));
        assert_eq!(at(-1, -1, CoordUnit::Pixel).cell_pos(CELL), (-1, -1));
        assert_eq!(at(2, 1, CoordUnit::Cell).cell_pos(CELL), (2, 1));
        let m = at(25, 30, CoordUnit::Pixel).offset(1, 1, CELL);
        assert_eq!((m.x, m.y), (15, 10));
        let m = at(2, 1, CoordUnit::Cell).offset(1, 1, CELL);
        assert_eq!((m.x, m.y), (1, 0));
        let m = at(2, 1, CoordUnit::Cell).in_pixels(CELL);
        assert_eq!((m.x, m.y, m.unit), (20, 20, CoordUnit::Pixel));
    }

    #[test]
    fn small_wheel_deltas_add_up_to_whole_rows() {
        let by = |dy| MouseEventKind::ScrollBy { dx: 0, dy, unit: ScrollUnit::Pixel, phase: ScrollPhase::Touch };
        let mut acc = WheelAccumulator::default();
        // 6 pixels (600 hundredths) of 20-pixel rows: 0.3 a time.
        let mut rows = Vec::new();
        for _ in 0..5 {
            acc.add(by(600).wheel_rows(CELL.height).unwrap());
            rows.push(acc.take(1.0));
        }
        assert_eq!(rows, [0, 0, 0, 1, 0]);
        acc.add(-2.5);
        assert_eq!(acc.take(1.0), -2);
        assert_eq!(MouseEventKind::Scroll(ScrollDirection::Up).wheel_rows(20), Some(-3.0));
        assert_eq!(MouseEventKind::Move.wheel_rows(20), None);
    }
}
