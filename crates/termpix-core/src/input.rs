//! Input events shared by every frontend: keys and mouse, independent of how
//! they were read (terminal escape sequences in `termpix::input`, or a window
//! system).

use std::fmt;

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

impl MouseEventKind {
    /// Any kind of scrolling.
    pub fn is_scroll(&self) -> bool {
        matches!(self, Self::Scroll(_) | Self::ScrollBy { .. })
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
