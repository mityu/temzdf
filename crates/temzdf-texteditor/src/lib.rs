//! A framework for building text editors (doc/design/text-editor.md): the
//! text and positions in it, edits, the cursor and selection, decorations
//! and marks, the undo tree and motions. Editors themselves are scripts on
//! top (Vim-like editing as Evil is in Emacs); nothing here knows about Vim,
//! terminals or the session.
//!
//! Offsets inside are `char` indices into the text (a `ropey` rope, LF the
//! only line break); scripts see [`Position`]s, a line and a UTF-16 column,
//! converted at the edge by [`Text`].

mod buffer;
mod cursor;
mod edit;
mod layout;
mod marks;
mod motion;
mod render;
mod text;
mod undo;
mod view;

pub use buffer::{Buffer, EditError, Edited, Origin};
pub use cursor::{Cursor, SelectionKind};
pub use edit::{Change, Gravity, TextEdit, map_offset};
pub use layout::{Glyph, GlyphKind, Layout, LayoutOptions, Row, grapheme_width};
pub use marks::{Mark, MarkId, MarkSpec, Marks};
pub use motion::{Class, Unit, WordChars, char_class, next_grapheme, position_of, prev_grapheme};
pub use render::{Drawn, Highlights, Style, render};
pub use text::{LineEnding, Position, Range, Text};
pub use undo::{StepKind, UndoTree};
pub use view::{NumberMode, Reveal, View, ViewUnit};
