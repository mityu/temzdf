//! Terminals: a program on a pty, its output interpreted by Ghostty's
//! terminal emulation (`libghostty-vt`) and shown as text in a
//! `temzdf-scene` frame. See doc/session-server.md (step 3).
//!
//! A [`TermBuffer`] is the program and its screen; a [`TermView`] is one
//! pane showing it (several panes can show one terminal, as Vim's windows
//! show one buffer). The terminal is as large as the largest view (as in
//! Neovim); smaller views show its bottom left part.
//!
//! The pty is read on a thread that hands the bytes to the caller's [`Sink`];
//! everything else (libghostty's handles are `!Send`) happens on the caller's
//! thread: [`TermBuffer::feed`] the bytes, then build a [`TermView::frame`].

mod graphics;
pub mod keys;

mod pty;

use std::cell::RefCell;
use std::io;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use libghostty_vt::fmt::Format;
use libghostty_vt::render::{CellIterator, CursorVisualStyle, Dirty, RowIterator};
use libghostty_vt::screen::CellWide;
use libghostty_vt::selection::FormatOptions;
use libghostty_vt::selection::gesture::{
    Autoscroll, AutoscrollTickEvent, DragEvent, Geometry as GestureGeometry, Gesture, PressEvent, ReleaseEvent,
};
use libghostty_vt::style::{StyleColor, Underline as GUnderline};
use libghostty_vt::terminal::{Mode, Point, PointCoordinate, ScrollViewport};
use libghostty_vt::{RenderState, Terminal, TerminalOptions, focus, key, mouse, paste};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use temzdf_scene::{Attrs, Cell, Color, CursorShape, Effect, Frame, Images, Layer, Rect, TextGrid, Underline};
use termpix_core::geom::Geometry;
use termpix_core::input::{CoordUnit, KeyEvent, Modifiers, MouseButton, MouseEvent, MouseEventKind, ScrollDirection};

/// What the pty's reader thread reports.
pub enum TermEvent {
    Output(Vec<u8>),
    /// The program exited (jobs it left may still hold the pty).
    Exited,
}

/// Where the reader thread sends events (returns false once nobody listens).
pub type Sink = Arc<dyn Fn(TermEvent) -> bool + Send + Sync>;

/// What to run in a pane.
#[derive(Clone, Debug, Default)]
pub struct Spawn {
    /// Program and arguments; the user's `$SHELL` when `None`.
    pub argv: Option<Vec<String>>,
    pub cwd: Option<PathBuf>,
    /// Extra environment (e.g. `TEMZDF_SESSION_NAME`).
    pub env: Vec<(String, String)>,
}

/// Lines of scrollback kept per terminal.
const SCROLLBACK: usize = 10_000;
/// Rows the wheel scrolls through history per step.
const WHEEL_ROWS: isize = 3;
/// Clicks closer together than this count as double and triple clicks.
const MULTI_CLICK: Duration = Duration::from_millis(500);
/// How often a selection dragged past the top or bottom scrolls a row.
const AUTOSCROLL_EVERY: Duration = Duration::from_millis(50);

/// A mouse selection: libghostty's gesture state machine (single click and
/// drag selects cells, double click words, triple click lines) and its
/// reusable events.
struct Selecting {
    gesture: Gesture<'static>,
    press: PressEvent<'static>,
    drag: DragEvent<'static>,
    release: ReleaseEvent<'static>,
    tick: AutoscrollTickEvent<'static>,
    /// The left button is down for a selection.
    active: bool,
    /// Where the pointer is (terminal pixels), for autoscrolling.
    pointer: (f32, f32),
    /// When to scroll the next row while dragged past an edge.
    autoscroll_at: Option<Instant>,
    /// A selection is shown.
    shown: bool,
    /// The OSC 8 link under the press, opened if it was a plain click.
    link: Option<String>,
}

pub struct TermBuffer {
    terminal: Terminal<'static, 'static>,
    render: RenderState<'static>,
    rows_it: RowIterator<'static>,
    cells_it: CellIterator<'static>,
    key_encoder: key::Encoder<'static>,
    mouse_encoder: mouse::Encoder<'static>,
    pty: pty::Pty,
    input: pty::Input,
    geometry: Geometry,
    /// The text of the viewport, updated from libghostty's dirty rows.
    grid: Vec<Vec<Cell>>,
    effects: Rc<RefCell<Vec<Effect>>>,
    /// The view is scrolled back into history.
    scrolled: bool,
    exited: bool,
    selecting: Selecting,
    /// The pointer is over an OSC 8 link.
    hover_link: bool,
    graphics: graphics::Graphics,
    born: Instant,
}

impl TermBuffer {
    /// Start `spawn` on a new pty of `geometry`'s size.
    pub fn spawn(spawn: &Spawn, geometry: Geometry, sink: Sink) -> io::Result<Self> {
        let pair = native_pty_system().openpty(pty_size(geometry)).map_err(io::Error::other)?;
        let argv = match &spawn.argv {
            Some(a) if !a.is_empty() => a.clone(),
            _ => vec![std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/sh".into())],
        };
        let mut cmd = CommandBuilder::new(&argv[0]);
        cmd.args(&argv[1..]);
        if let Some(dir) = spawn.cwd.clone().or_else(|| std::env::current_dir().ok()) {
            cmd.cwd(dir);
        }
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        cmd.env("TERM_PROGRAM", "temzdf");
        // Programs in the pane are not inside the multiplexer the server
        // may have been started from.
        for var in ["TMUX", "TMUX_PANE", "ZELLIJ", "ZELLIJ_SESSION_NAME", "KITTY_WINDOW_ID", "WEZTERM_PANE"] {
            cmd.env_remove(var);
        }
        for (k, v) in &spawn.env {
            cmd.env(k, v);
        }
        let child = pair.slave.spawn_command(cmd).map_err(io::Error::other)?;
        drop(pair.slave);
        let pty = pty::Pty::start(pair.master, child, sink)?;
        let input = pty.input();

        let mut terminal = Terminal::new(TerminalOptions {
            cols: geometry.cols.max(1),
            rows: geometry.rows.max(1),
            max_scrollback: SCROLLBACK,
        })
        .map_err(ghostty_err)?;
        terminal
            .resize(geometry.cols.max(1), geometry.rows.max(1), geometry.cell.width, geometry.cell.height)
            .map_err(ghostty_err)?;
        // Images from programs (kitty graphics), by any medium kitty takes.
        terminal.set_kitty_image_storage_limit(graphics::STORAGE_LIMIT).map_err(ghostty_err)?;
        let _ = terminal.set_kitty_image_from_file_allowed(true);
        let _ = terminal.set_kitty_image_from_temp_file_allowed(true);
        let _ = terminal.set_kitty_image_from_shared_mem_allowed(true);
        // Replies to the program's queries (cursor position, device
        // attributes, ...) go back through the pty.
        let replies = input.clone();
        terminal
            .on_pty_write(move |_, data| {
                replies.write(data);
            })
            .map_err(ghostty_err)?;
        let effects: Rc<RefCell<Vec<Effect>>> = Rc::default();
        let e = effects.clone();
        terminal
            .on_clipboard_write(move |_, write| {
                for content in write.contents() {
                    if content.mime.starts_with("text/plain") {
                        e.borrow_mut().push(Effect::Copy(content.data.to_owned()));
                        break;
                    }
                }
                Ok(())
            })
            .map_err(ghostty_err)?;
        Ok(Self {
            terminal,
            render: RenderState::new().map_err(ghostty_err)?,
            rows_it: RowIterator::new().map_err(ghostty_err)?,
            cells_it: CellIterator::new().map_err(ghostty_err)?,
            key_encoder: key::Encoder::new().map_err(ghostty_err)?,
            mouse_encoder: mouse::Encoder::new().map_err(ghostty_err)?,
            pty,
            input,
            geometry,
            grid: Vec::new(),
            effects,
            scrolled: false,
            exited: false,
            selecting: Selecting {
                gesture: Gesture::new().map_err(ghostty_err)?,
                press: PressEvent::new().map_err(ghostty_err)?,
                drag: DragEvent::new().map_err(ghostty_err)?,
                release: ReleaseEvent::new().map_err(ghostty_err)?,
                tick: AutoscrollTickEvent::new().map_err(ghostty_err)?,
                active: false,
                pointer: (0.0, 0.0),
                autoscroll_at: None,
                shown: false,
                link: None,
            },
            hover_link: false,
            graphics: graphics::Graphics::new().map_err(ghostty_err)?,
            born: Instant::now(),
        })
    }

    /// Output read from the pty.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.terminal.vt_write(bytes);
        self.pty.consumed(bytes.len());
    }

    /// The program exited.
    pub fn set_exited(&mut self) {
        self.exited = true;
    }

    /// The program's process id.
    pub fn pid(&self) -> Option<i32> {
        self.pty.pid()
    }

    pub fn exited(&self) -> bool {
        self.exited
    }

    /// Input for the program; dropped when it has stopped reading for long
    /// (never blocks the session).
    fn write(&self, bytes: &[u8]) {
        self.input.write(bytes);
    }

    /// Write `text` to the program as if typed (for scripts, like Vim's
    /// `chansend()`).
    pub fn send_text(&mut self, text: &str) -> bool {
        let redraw = self.scroll_to_bottom();
        self.write(text.as_bytes());
        redraw
    }

    pub fn geometry(&self) -> Geometry {
        self.geometry
    }

    pub fn resize(&mut self, geometry: Geometry) {
        if geometry == self.geometry {
            return;
        }
        self.geometry = geometry;
        self.pty.resize(pty_size(geometry));
        let _ =
            self.terminal.resize(geometry.cols.max(1), geometry.rows.max(1), geometry.cell.width, geometry.cell.height);
        self.grid.clear();
    }

    /// Effects the program asked for (clipboard writes).
    pub fn take_effects(&mut self) -> Vec<Effect> {
        std::mem::take(&mut self.effects.borrow_mut())
    }

    /// The program's window title, if it set one.
    pub fn title(&self) -> Option<String> {
        self.terminal.title().ok().filter(|t| !t.is_empty()).map(str::to_owned)
    }

    /// The working directory the shell reported (OSC 7), if any.
    pub fn pwd(&self) -> Option<PathBuf> {
        let pwd = self.terminal.pwd().ok().filter(|p| !p.is_empty())?;
        // OSC 7 carries a file:// URL.
        let path = pwd.strip_prefix("file://").map_or(pwd, |rest| rest.find('/').map_or(rest, |i| &rest[i..]));
        Some(PathBuf::from(path))
    }

    /// A key typed while a pane showing it has the focus.
    pub fn key(&mut self, k: &KeyEvent) -> bool {
        let redraw = self.clear_selection() | self.scroll_to_bottom();
        let out = keys::encode(&mut self.key_encoder, &self.terminal, k);
        self.write(&out);
        redraw
    }

    /// Leave the history for the live screen. Returns whether it moved.
    pub fn scroll_to_bottom(&mut self) -> bool {
        if std::mem::take(&mut self.scrolled) {
            self.terminal.scroll_viewport(ScrollViewport::Bottom);
            return true;
        }
        false
    }

    /// Bracketed paste when the program asked for it.
    pub fn paste(&mut self, text: &str) -> bool {
        let redraw = self.clear_selection() | self.scroll_to_bottom();
        let bracketed = self.terminal.mode(Mode::BRACKETED_PASTE).unwrap_or(false);
        let mut data = text.as_bytes().to_vec();
        let mut buf = vec![0u8; data.len() + 16];
        if let Ok(n) = paste::encode(&mut data, bracketed, &mut buf) {
            self.write(&buf[..n]);
        }
        redraw
    }

    /// A pane showing it gained or lost the focus (reported when the program asked).
    pub fn focus(&mut self, gained: bool) {
        if self.terminal.mode(Mode::FOCUS_EVENT).unwrap_or(false) {
            let mut buf = [0u8; 8];
            let event = if gained { focus::Event::Gained } else { focus::Event::Lost };
            if let Ok(n) = event.encode(&mut buf) {
                self.write(&buf[..n]);
            }
        }
    }

    /// A mouse event in terminal coordinates. Programs that track the mouse
    /// get it, except that Shift and the left button select text (as does the
    /// left button alone for other programs, or with `select`: the session's
    /// operating mode); otherwise the wheel scrolls through history.
    pub fn mouse(&mut self, m: &MouseEvent, select: bool) -> bool {
        let cell = self.geometry.cell;
        let (x, y) = match m.unit {
            CoordUnit::Pixel => (m.x as f32, m.y as f32),
            CoordUnit::Cell => ((m.x as f32 + 0.5) * cell.width as f32, (m.y as f32 + 0.5) * cell.height as f32),
        };
        let tracking = self.terminal.is_mouse_tracking().unwrap_or(false);
        let mut redraw = false;
        if matches!(m.kind, MouseEventKind::Move | MouseEventKind::Drag(_)) {
            let over = self.link_at(x, y).is_some();
            redraw = over != std::mem::replace(&mut self.hover_link, over);
        } else if m.kind == MouseEventKind::Leave {
            redraw = std::mem::take(&mut self.hover_link);
        }
        match m.kind {
            MouseEventKind::Press(MouseButton::Left)
                if !tracking || select || m.modifiers.contains(Modifiers::SHIFT) =>
            {
                return self.select_press(x, y) | redraw;
            }
            MouseEventKind::Drag(MouseButton::Left) if self.selecting.active => return self.select_drag(x, y) | redraw,
            MouseEventKind::Release(MouseButton::Left) if self.selecting.active => {
                return self.select_release() | redraw;
            }
            _ => {}
        }
        if tracking {
            let (action, button) = match m.kind {
                MouseEventKind::Press(b) => (mouse::Action::Press, Some(button(b))),
                MouseEventKind::Release(b) => (mouse::Action::Release, Some(button(b))),
                MouseEventKind::Drag(b) => (mouse::Action::Motion, Some(button(b))),
                MouseEventKind::Move => (mouse::Action::Motion, None),
                MouseEventKind::Scroll(d) => (
                    mouse::Action::Press,
                    Some(match d {
                        ScrollDirection::Up => mouse::Button::Four,
                        ScrollDirection::Down => mouse::Button::Five,
                        ScrollDirection::Left => mouse::Button::Six,
                        ScrollDirection::Right => mouse::Button::Seven,
                    }),
                ),
                MouseEventKind::Leave => return redraw,
            };
            let Ok(mut event) = mouse::Event::new() else { return false };
            let mods = keys::mods(m.modifiers);
            event.set_action(action).set_button(button).set_mods(mods).set_position(mouse::Position { x, y });
            self.mouse_encoder.set_options_from_terminal(&self.terminal);
            self.mouse_encoder.set_size(mouse::EncoderSize {
                screen_width: self.geometry.width_px(),
                screen_height: self.geometry.height_px(),
                cell_width: cell.width.max(1),
                cell_height: cell.height.max(1),
                padding_top: 0,
                padding_bottom: 0,
                padding_right: 0,
                padding_left: 0,
            });
            self.mouse_encoder
                .set_any_button_pressed(matches!(m.kind, MouseEventKind::Press(_) | MouseEventKind::Drag(_)));
            let mut out = Vec::new();
            if self.mouse_encoder.encode_to_vec(&event, &mut out).is_ok() {
                self.write(&out);
            }
            return redraw;
        }
        redraw
            | match m.kind {
                MouseEventKind::Scroll(ScrollDirection::Up) => {
                    self.terminal.scroll_viewport(ScrollViewport::Delta(-WHEEL_ROWS));
                    self.scrolled = true;
                    true
                }
                MouseEventKind::Scroll(ScrollDirection::Down) => {
                    self.terminal.scroll_viewport(ScrollViewport::Delta(WHEEL_ROWS));
                    true
                }
                _ => false,
            }
    }

    /// The cell under a terminal pixel, clamped into the screen.
    fn cell_at(&self, x: f32, y: f32) -> PointCoordinate {
        let g = self.geometry;
        let col = (x / g.cell.width.max(1) as f32).floor().clamp(0.0, g.cols.saturating_sub(1) as f32);
        let row = (y / g.cell.height.max(1) as f32).floor().clamp(0.0, g.rows.saturating_sub(1) as f32);
        PointCoordinate { x: col as u16, y: row as u32 }
    }

    fn gesture_geometry(&self) -> GestureGeometry {
        let g = self.geometry;
        GestureGeometry {
            columns: g.cols.max(1) as u32,
            cell_width: g.cell.width.max(1),
            padding_left: 0,
            screen_height: g.height_px().max(1),
        }
    }

    /// The OSC 8 link of the cell under a terminal pixel.
    fn link_at(&self, x: f32, y: f32) -> Option<String> {
        let g = self.geometry;
        if x < 0.0 || y < 0.0 || x >= g.width_px() as f32 || y >= g.height_px() as f32 {
            return None;
        }
        let grid = self.terminal.grid_ref(Point::Viewport(self.cell_at(x, y))).ok()?;
        let mut buf = vec![0u8; 2048];
        let n = match grid.hyperlink_uri(&mut buf) {
            Err(libghostty_vt::Error::OutOfSpace { required }) => {
                buf.resize(required.max(1 << 16), 0);
                grid.hyperlink_uri(&mut buf).ok()?
            }
            other => other.ok()?,
        };
        (n > 0).then(|| String::from_utf8_lossy(&buf[..n]).into_owned())
    }

    /// The pointer is over a link (for its shape).
    pub fn hovering_link(&self) -> bool {
        self.hover_link
    }

    fn select_press(&mut self, x: f32, y: f32) -> bool {
        self.selecting.link = self.link_at(x, y);
        let at = self.cell_at(x, y);
        let time = self.born.elapsed();
        let distance = self.geometry.cell.width as f64;
        let sel = &mut self.selecting;
        let t = &self.terminal;
        let Ok(grid) = t.grid_ref(Point::Viewport(at)) else { return false };
        let ev = sel
            .press
            .set_position(x as f64, y as f64)
            .and_then(|e| e.set_time(time))
            .and_then(|e| e.set_repeat_interval(MULTI_CLICK))
            .and_then(|e| e.set_repeat_distance(distance));
        if ev.is_err() {
            return false;
        }
        // A single click selects nothing (it clears the selection) until it
        // drags; double and triple clicks select a word or a line at once.
        let selection = sel.press.apply(&mut sel.gesture, t, grid).ok().flatten();
        let _ = t.set_selection(selection.as_ref());
        let redraw = sel.shown || selection.is_some();
        sel.shown = selection.is_some();
        sel.active = true;
        sel.pointer = (x, y);
        if redraw {
            self.grid.clear();
        }
        redraw
    }

    fn select_drag(&mut self, x: f32, y: f32) -> bool {
        let at = self.cell_at(x, y);
        let geometry = self.gesture_geometry();
        let sel = &mut self.selecting;
        let t = &self.terminal;
        sel.pointer = (x, y);
        let Ok(grid) = t.grid_ref(Point::Viewport(at)) else { return false };
        if sel.drag.set_position(x as f64, y as f64).is_err() {
            return false;
        }
        let Ok(Some(selection)) = sel.drag.apply(&mut sel.gesture, t, grid, geometry) else { return false };
        let _ = t.set_selection(Some(&selection));
        sel.shown = true;
        let edge = !matches!(sel.gesture.autoscroll(t), Ok(Autoscroll::None) | Err(_));
        sel.autoscroll_at = edge.then(|| sel.autoscroll_at.unwrap_or_else(|| Instant::now() + AUTOSCROLL_EVERY));
        self.grid.clear();
        true
    }

    /// The button went up: the selection is copied (to the clipboard of the
    /// client, OSC 52 in a terminal).
    fn select_release(&mut self) -> bool {
        let sel = &mut self.selecting;
        let t = &self.terminal;
        // A plain click on a link opens it.
        let click = !sel.gesture.dragged(t).unwrap_or(true) && sel.gesture.click_count(t).unwrap_or(0) == 1;
        let _ = sel.release.apply(&mut sel.gesture, t, None);
        sel.active = false;
        sel.autoscroll_at = None;
        if let Some(uri) = sel.link.take()
            && click
        {
            self.effects.borrow_mut().push(Effect::OpenUri { uri, command: None });
            return false;
        }
        if !sel.shown {
            return false;
        }
        let options = FormatOptions::new().with_emit_format(Format::Plain).with_unwrap(true).with_trim(true);
        if let Ok(Some(bytes)) = t.format_selection_alloc(None, options) {
            let text = String::from_utf8_lossy(&bytes).into_owned();
            if !text.is_empty() {
                self.effects.borrow_mut().push(Effect::Copy(text));
            }
        }
        false
    }

    /// Forget the selection (typing does). Returns whether one was shown.
    pub fn clear_selection(&mut self) -> bool {
        if !std::mem::take(&mut self.selecting.shown) {
            return false;
        }
        let _ = self.terminal.set_selection(None);
        self.grid.clear();
        true
    }

    /// The selection is dragged past the top or bottom: when to scroll.
    pub fn autoscroll_deadline(&self) -> Option<Instant> {
        self.selecting.autoscroll_at
    }

    /// Scroll a row towards where the selection is dragged, extending it.
    pub fn autoscroll(&mut self, now: Instant) -> bool {
        let Some(at) = self.selecting.autoscroll_at else { return false };
        if now < at {
            return false;
        }
        let direction = match self.selecting.gesture.autoscroll(&self.terminal) {
            Ok(Autoscroll::Up) => -1,
            Ok(Autoscroll::Down) => 1,
            _ => {
                self.selecting.autoscroll_at = None;
                return false;
            }
        };
        self.terminal.scroll_viewport(ScrollViewport::Delta(direction));
        if direction < 0 {
            self.scrolled = true;
        }
        let (x, y) = self.selecting.pointer;
        let at = self.cell_at(x, y);
        let geometry = self.gesture_geometry();
        let sel = &mut self.selecting;
        let t = &self.terminal;
        if let Ok(Some(selection)) = sel.tick.apply(&mut sel.gesture, t, at, geometry) {
            let _ = t.set_selection(Some(&selection));
        }
        sel.autoscroll_at = Some(now + AUTOSCROLL_EVERY);
        self.grid.clear();
        true
    }

    /// Rows above the bottom and rows of history, while the view is scrolled
    /// back (`None` at the bottom).
    pub fn scroll_position(&self) -> Option<(u64, u64)> {
        if !self.scrolled {
            return None;
        }
        let bar = self.terminal.scrollbar().ok()?;
        let above = bar.total.saturating_sub(bar.offset + bar.len);
        (above > 0).then(|| (above, bar.total.saturating_sub(bar.len)))
    }

    /// The images programs placed (kitty graphics) as layers in terminal
    /// pixels, their pixels in `images`.
    pub fn layers(&mut self, images: &mut Images) -> Vec<Layer> {
        self.graphics.layers(&self.terminal, self.geometry.cell, images)
    }

    /// No pane shows the terminal: its images leave the store.
    pub fn release(&mut self, images: &mut Images) {
        self.graphics.release(images);
    }

    /// The whole screen now.
    pub fn frame(&mut self) -> Frame {
        let mut cursor = None;
        let mut cursor_shape = CursorShape::Default;
        if let Ok(snapshot) = self.render.update(&self.terminal) {
            let full = matches!(snapshot.dirty(), Ok(Dirty::Full)) || self.grid.is_empty();
            let cols = self.geometry.cols as usize;
            let rows = self.geometry.rows as usize;
            self.grid.resize_with(rows, || vec![Cell::blank(); cols]);
            if let Ok(mut row_iter) = self.rows_it.update(&snapshot) {
                let mut y = 0;
                while let Some(row) = row_iter.next() {
                    if y >= rows {
                        break;
                    }
                    if full || row.dirty().unwrap_or(true) {
                        let mut line = Vec::with_capacity(cols);
                        if let Ok(mut cell_iter) = self.cells_it.update(row) {
                            while let Some(c) = cell_iter.next() {
                                line.push(convert(c));
                            }
                        }
                        line.resize_with(cols, Cell::blank);
                        // The selection shows inverted.
                        if let Ok(Some(sel)) = row.selection() {
                            for c in line.iter_mut().take(sel.end_x as usize + 1).skip(sel.start_x as usize) {
                                c.attrs.inverse = !c.attrs.inverse;
                            }
                        }
                        self.grid[y] = line;
                        let _ = row.set_dirty(false);
                    }
                    y += 1;
                }
            }
            let _ = snapshot.set_dirty(Dirty::Clean);
            if snapshot.cursor_visible().unwrap_or(false)
                && let Ok(Some(c)) = snapshot.cursor_viewport()
            {
                cursor = Some((c.x, c.y));
                let blink = snapshot.cursor_blinking().unwrap_or(false);
                cursor_shape = match snapshot.cursor_visual_style() {
                    Ok(CursorVisualStyle::Bar) => CursorShape::Bar { blink },
                    Ok(CursorVisualStyle::Underline) => CursorShape::Underline { blink },
                    Ok(CursorVisualStyle::Block) => CursorShape::Block { blink },
                    _ => CursorShape::Default,
                };
            }
        }
        Frame {
            text: vec![TextGrid { col: 0, row: 0, rows: self.grid.clone() }],
            cursor,
            cursor_shape,
            title: self.title().unwrap_or_else(|| "shell".into()),
            ..Default::default()
        }
    }
}

/// One pane showing a [`TermBuffer`].
#[derive(Clone, Copy, Debug)]
pub struct TermView {
    pub geometry: Geometry,
}

impl TermView {
    pub fn new(geometry: Geometry) -> Self {
        Self { geometry }
    }

    /// Rows of the terminal above the view (it shows the bottom).
    fn top(&self, term: &TermBuffer) -> u16 {
        term.geometry.rows.saturating_sub(self.geometry.rows)
    }

    /// What the view shows: the terminal's bottom left part of the view's
    /// size, images included (their pixels in `images`).
    pub fn frame(&self, term: &mut TermBuffer, images: &mut Images) -> Frame {
        let mut frame = term.frame();
        let top = self.top(term) as usize;
        let dy = top as i32 * term.geometry.cell.height as i32;
        frame.layers = term
            .layers(images)
            .into_iter()
            .map(|l| Layer { dst: Rect::new(l.dst.x, l.dst.y - dy, l.dst.w, l.dst.h), ..l })
            .collect();
        let (cols, rows) = (self.geometry.cols as usize, self.geometry.rows as usize);
        if let Some(grid) = frame.text.first_mut()
            && (top > 0 || grid.rows.len() != rows || grid.rows.first().is_some_and(|r| r.len() != cols))
        {
            let mut out: Vec<Vec<Cell>> = grid.rows.drain(..).skip(top).take(rows).collect();
            out.resize_with(rows, Vec::new);
            for row in &mut out {
                row.truncate(cols);
                row.resize_with(cols, Cell::blank);
                // A wide character cut in half at the view's edge.
                temzdf_scene::mend_wide(row);
            }
            grid.rows = out;
        }
        frame.cursor = frame.cursor.and_then(|(c, r)| {
            let r = r.checked_sub(top as u16)?;
            (c < self.geometry.cols && r < self.geometry.rows).then_some((c, r))
        });
        // Scrolled back: how far, at the top right (as tmux's copy mode).
        if let Some((above, history)) = term.scroll_position()
            && let Some(row) = frame.text.first_mut().and_then(|g| g.rows.first_mut())
        {
            let label = format!("[{above}/{history}]");
            let start = row.len().saturating_sub(label.chars().count());
            for (c, ch) in row[start..].iter_mut().zip(label.chars()) {
                *c = Cell {
                    text: ch.to_string(),
                    width: 1,
                    attrs: Attrs { inverse: true, ..Attrs::default() },
                    ..Cell::blank()
                };
            }
            // The label may cover half of a wide character.
            temzdf_scene::mend_wide(row);
        }
        frame
    }

    /// A mouse event in view coordinates (see [`TermBuffer::mouse`]).
    pub fn mouse(&self, term: &mut TermBuffer, m: &MouseEvent, select: bool) -> bool {
        let top = self.top(term) as i32;
        let dy = match m.unit {
            CoordUnit::Pixel => top * term.geometry.cell.height as i32,
            CoordUnit::Cell => top,
        };
        term.mouse(&MouseEvent { y: m.y + dy, ..*m }, select)
    }
}

fn pty_size(g: Geometry) -> PtySize {
    PtySize {
        rows: g.rows.max(1),
        cols: g.cols.max(1),
        pixel_width: g.width_px().min(u16::MAX as u32) as u16,
        pixel_height: g.height_px().min(u16::MAX as u32) as u16,
    }
}

fn ghostty_err(e: libghostty_vt::Error) -> io::Error {
    io::Error::other(format!("libghostty-vt: {e:?}"))
}

fn button(b: MouseButton) -> mouse::Button {
    match b {
        MouseButton::Left => mouse::Button::Left,
        MouseButton::Middle => mouse::Button::Middle,
        MouseButton::Right => mouse::Button::Right,
        MouseButton::Other(8) => mouse::Button::Eight,
        MouseButton::Other(9) => mouse::Button::Nine,
        MouseButton::Other(_) => mouse::Button::Unknown,
    }
}

fn color(c: StyleColor) -> Color {
    match c {
        StyleColor::None => Color::Default,
        StyleColor::Palette(i) => Color::Indexed(i.0),
        StyleColor::Rgb(c) => Color::Rgb([c.r, c.g, c.b]),
    }
}

/// One libghostty cell as a scene cell.
fn convert(c: &libghostty_vt::render::CellIteration<'_, '_>) -> Cell {
    let width = match c.raw_cell().and_then(|r| r.wide()) {
        Ok(CellWide::Wide) => 2,
        Ok(CellWide::SpacerTail) => 0,
        _ => 1,
    };
    let text =
        if width == 0 { String::new() } else { c.graphemes().map(|g| g.into_iter().collect()).unwrap_or_default() };
    let Ok(style) = c.style() else { return Cell { text, width, ..Cell::default() } };
    let attrs = Attrs {
        bold: style.bold,
        faint: style.faint,
        italic: style.italic,
        underline: match style.underline {
            GUnderline::None => Underline::None,
            GUnderline::Single => Underline::Single,
            GUnderline::Double => Underline::Double,
            GUnderline::Curly => Underline::Curly,
            GUnderline::Dotted => Underline::Dotted,
            GUnderline::Dashed => Underline::Dashed,
            _ => Underline::Single,
        },
        blink: style.blink,
        inverse: style.inverse,
        invisible: style.invisible,
        strikethrough: style.strikethrough,
        overline: style.overline,
    };
    Cell { text, width, fg: color(style.fg_color), bg: color(style.bg_color), attrs }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;
    use termpix_core::geom::CellSize;
    use termpix_core::input::{KeyCode, Modifiers};

    const GEOMETRY: Geometry = Geometry { cols: 40, rows: 6, cell: CellSize { width: 10, height: 20 } };

    /// Run `argv` in a pane and feed it until `done` holds for its text (or
    /// the program exits). Returns the pane and its rows.
    fn run(
        argv: &[&str],
        mut input: impl FnMut(&mut TermBuffer),
        done: impl Fn(&[String]) -> bool,
    ) -> (TermBuffer, Vec<String>) {
        let (tx, rx) = mpsc::channel();
        let tx = std::sync::Mutex::new(tx);
        let sink: Sink = Arc::new(move |e| tx.lock().unwrap().send(e).is_ok());
        let spawn = Spawn { argv: Some(argv.iter().map(|s| s.to_string()).collect()), ..Default::default() };
        let mut pane = TermBuffer::spawn(&spawn, GEOMETRY, sink).unwrap();
        input(&mut pane);
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            let rows = text(&mut pane);
            if done(&rows) || pane.exited() || std::time::Instant::now() > deadline {
                return (pane, rows);
            }
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(TermEvent::Output(b)) => pane.feed(&b),
                Ok(TermEvent::Exited) => pane.set_exited(),
                Err(_) => {}
            }
        }
    }

    fn text(pane: &mut TermBuffer) -> Vec<String> {
        let frame = pane.frame();
        frame.text[0]
            .rows
            .iter()
            .map(|r| {
                r.iter()
                    .map(|c| if c.text.is_empty() { " ".into() } else { c.text.clone() })
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect()
    }

    #[test]
    fn shows_program_output_with_colours() {
        let (mut pane, rows) = run(
            &["sh", "-c", "printf 'hello \\033[31mred\\033[0m\\n\\033[38;2;1;2;3mrgb'; sleep 5"],
            |_| {},
            |r| r[1].contains("rgb"),
        );
        assert_eq!(&rows[..2], ["hello red", "rgb"]);
        let frame = pane.frame();
        let row0 = &frame.text[0].rows[0];
        assert_eq!(row0[6].fg, Color::Indexed(1), "palette colours stay indices");
        assert_eq!(row0[0].fg, Color::Default);
        assert_eq!(frame.text[0].rows[1][0].fg, Color::Rgb([1, 2, 3]));
    }

    #[test]
    fn keys_reach_the_program() {
        let (_, rows) = run(
            &["sh", "-c", "stty -echo; read line; echo \"got:$line\"; sleep 5"],
            |p| {
                for c in "hi".chars() {
                    p.key(&KeyEvent::new(KeyCode::Char(c), Modifiers::NONE));
                }
                p.key(&KeyEvent::new(KeyCode::Enter, Modifiers::NONE));
            },
            |r| r.iter().any(|l| l.contains("got:")),
        );
        assert!(rows.iter().any(|l| l == "got:hi"), "{rows:?}");
    }

    #[test]
    fn smaller_views_show_the_bottom_left() {
        let (mut term, _) = run(&["sh", "-c", "printf 'a\\nb\\nc\\nd\\ne\\nf'; sleep 5"], |_| {}, |r| r[5] == "f");
        let small = TermView::new(Geometry { cols: 3, rows: 2, ..GEOMETRY });
        let frame = small.frame(&mut term, &mut Images::new());
        let rows: Vec<String> =
            frame.text[0].rows.iter().map(|r| r.iter().map(|c| c.text.as_str()).collect()).collect();
        assert_eq!(rows, ["e", "f"]);
        assert_eq!(frame.text[0].rows[0].len(), 3);
        // The cursor is right of f (when libghostty shows it).
        assert!(matches!(frame.cursor, None | Some((1, 1))), "{:?}", frame.cursor);
        let narrow = TermView::new(Geometry { cols: 1, rows: 2, ..GEOMETRY }).frame(&mut term, &mut Images::new());
        assert_eq!(narrow.cursor, None, "outside the view");
    }

    #[test]
    fn a_wide_character_cut_at_the_views_edge_goes_blank() {
        // "ab数" in a view 3 wide (on the last row: small views show the
        // bottom left): the wide character's first half is the last cell;
        // drawn, it would spill over (or wrap the screen).
        let (mut term, _) =
            run(&["sh", "-c", "printf '\\n\\n\\n\\n\\nab\\346\\225\\260'; sleep 5"], |_| {}, |r| r[5] == "ab数");
        let view = TermView::new(Geometry { cols: 3, rows: 2, ..GEOMETRY });
        let frame = view.frame(&mut term, &mut Images::new());
        let row = &frame.text[0].rows[1];
        assert_eq!(row.len(), 3);
        assert_eq!((row[2].text.as_str(), row[2].width), ("", 1), "{row:?}");
    }

    fn mouse(kind: MouseEventKind, col: u16, row: u16, modifiers: Modifiers) -> MouseEvent {
        // Pixel coordinates in the middle of the cell.
        let (x, y) = (col as i32 * 10 + 5, row as i32 * 20 + 10);
        MouseEvent { kind, x, y, unit: CoordUnit::Pixel, modifiers }
    }

    fn copies(term: &mut TermBuffer) -> Vec<String> {
        term.take_effects()
            .into_iter()
            .filter_map(|e| match e {
                Effect::Copy(t) => Some(t),
                _ => None,
            })
            .collect()
    }

    const LEFT: MouseButton = MouseButton::Left;

    #[test]
    fn dragging_selects_and_releasing_copies() {
        let (mut term, _) =
            run(&["sh", "-c", "printf 'hello world\\nsecond line'; sleep 5"], |_| {}, |r| r[1] == "second line");
        let none = Modifiers::NONE;
        term.mouse(&mouse(MouseEventKind::Press(LEFT), 6, 0, none), false);
        term.mouse(&mouse(MouseEventKind::Drag(LEFT), 2, 1, none), false);
        let frame = term.frame();
        assert!(frame.text[0].rows[0][6].attrs.inverse, "the selection shows inverted");
        assert!(!frame.text[0].rows[0][5].attrs.inverse);
        term.mouse(&mouse(MouseEventKind::Release(LEFT), 2, 1, none), false);
        // The cell under the pointer counts once the pointer passes its
        // middle (as in Ghostty): the drag ends before the c.
        assert_eq!(copies(&mut term), ["world\nse"]);
        // A double click selects a word; typing clears it.
        for kind in [MouseEventKind::Press(LEFT), MouseEventKind::Release(LEFT)].repeat(2) {
            term.mouse(&mouse(kind, 1, 0, none), false);
        }
        assert_eq!(copies(&mut term), ["hello"]);
        assert!(term.key(&KeyEvent::new(KeyCode::Char('x'), Modifiers::NONE)));
        assert!(!term.frame().text[0].rows[0][1].attrs.inverse);
        // A plain click copies nothing.
        term.mouse(&mouse(MouseEventKind::Press(LEFT), 1, 1, none), false);
        term.mouse(&mouse(MouseEventKind::Release(LEFT), 1, 1, none), false);
        assert!(copies(&mut term).is_empty());
    }

    #[test]
    fn programs_tracking_the_mouse_get_plain_drags_and_shift_selects() {
        // Mouse tracking (button events, SGR), then echo what arrives.
        let script = "stty raw -echo; printf 'ready\\033[?1002h\\033[?1006h\\n'; od -An -c";
        let (mut term, _) = run(&["sh", "-c", script], |_| {}, |r| r[0] == "ready");
        let none = Modifiers::NONE;
        term.mouse(&mouse(MouseEventKind::Press(LEFT), 0, 0, none), false);
        term.mouse(&mouse(MouseEventKind::Release(LEFT), 0, 0, none), false);
        assert!(copies(&mut term).is_empty());
        assert!(!term.selecting.active, "the program got it");
        // With Shift (or in the operating mode), temzdf selects.
        term.mouse(&mouse(MouseEventKind::Press(LEFT), 0, 0, Modifiers::SHIFT), false);
        term.mouse(&mouse(MouseEventKind::Drag(LEFT), 4, 0, Modifiers::SHIFT), false);
        term.mouse(&mouse(MouseEventKind::Release(LEFT), 4, 0, Modifiers::SHIFT), false);
        assert_eq!(copies(&mut term), ["read"]);
        // (Not a double click.)
        std::thread::sleep(MULTI_CLICK);
        term.mouse(&mouse(MouseEventKind::Press(LEFT), 1, 0, none), true);
        term.mouse(&mouse(MouseEventKind::Drag(LEFT), 2, 0, none), true);
        term.mouse(&mouse(MouseEventKind::Release(LEFT), 2, 0, none), true);
        assert_eq!(copies(&mut term), ["e"]);
    }

    #[test]
    fn clicking_a_link_opens_it() {
        let script = "printf 'see \\033]8;;https://example.org/a\\033\\\\here\\033]8;;\\033\\\\ now'; sleep 5";
        let (mut term, _) = run(&["sh", "-c", script], |_| {}, |r| r[0] == "see here now");
        let none = Modifiers::NONE;
        assert!(term.mouse(&mouse(MouseEventKind::Move, 5, 0, none), false), "the pointer changes");
        assert!(term.hovering_link());
        assert!(term.mouse(&mouse(MouseEventKind::Move, 1, 0, none), false));
        assert!(!term.hovering_link());
        term.mouse(&mouse(MouseEventKind::Press(LEFT), 5, 0, none), false);
        term.mouse(&mouse(MouseEventKind::Release(LEFT), 5, 0, none), false);
        let opened: Vec<_> = term
            .take_effects()
            .into_iter()
            .filter_map(|e| match e {
                Effect::OpenUri { uri, .. } => Some(uri),
                _ => None,
            })
            .collect();
        assert_eq!(opened, ["https://example.org/a"]);
        // Dragging over it selects instead.
        std::thread::sleep(MULTI_CLICK);
        term.mouse(&mouse(MouseEventKind::Press(LEFT), 4, 0, none), false);
        term.mouse(&mouse(MouseEventKind::Drag(LEFT), 8, 0, none), false);
        term.mouse(&mouse(MouseEventKind::Release(LEFT), 8, 0, none), false);
        assert_eq!(copies(&mut term), ["here"]);
    }

    #[test]
    fn images_from_programs_become_layers() {
        // A 2x2 RGBA image shown over 4x2 cells after "ab", then a 1x1 PNG.
        let rgba = "/wAA/wD/AP8AAP///////w==";
        let png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGPgEpEDAABoAD1UCKP3AAAAAElFTkSuQmCC";
        let script = format!(
            "printf 'ab\\033_Ga=T,i=7,f=32,s=2,v=2,c=4,r=2,q=2;{rgba}\\033\\\\\\n\\n\\033_Ga=T,i=8,f=100,q=2;{png}\\033\\\\done'; sleep 5"
        );
        let (mut term, _) = run(&["sh", "-c", &script], |_| {}, |r| r.iter().any(|l| l.ends_with("done")));
        let mut images = Images::new();
        let view = TermView::new(GEOMETRY);
        let frame = view.frame(&mut term, &mut images);
        let mut layers = frame.layers.clone();
        layers.sort_by_key(|l| l.dst.y);
        assert_eq!(layers.len(), 2, "{layers:?}");
        assert_eq!(layers[0].dst, Rect::new(20, 0, 40, 40), "at the cursor, scaled to 4x2 cells");
        assert_eq!(layers[0].src, Rect::new(0, 0, 2, 2));
        let (w, h, pixels) = images.get(layers[0].image).map(|i| (i.width, i.height, i.pixels.to_vec())).unwrap();
        assert_eq!((w, h, &pixels[..8]), (2, 2, &[255, 0, 0, 255, 0, 255, 0, 255][..]));
        let png_pixels = images.get(layers[1].image).map(|i| i.pixels.to_vec()).unwrap();
        assert_eq!(png_pixels, [10, 20, 30, 255], "PNG decoded");
        assert_eq!(layers[1].dst.w, 1);
        // Placed again next frame: the same images; gone with the terminal.
        let again = view.frame(&mut term, &mut images);
        assert_eq!(again.layers.len(), 2);
        assert_eq!(images.len(), 2);
        term.release(&mut images);
        assert_eq!(images.len(), 0);
    }

    #[test]
    fn scrolled_back_views_say_how_far() {
        let (mut term, _) = run(&["sh", "-c", "seq 1 20; sleep 5"], |_| {}, |r| r.iter().any(|l| l == "20"));
        let view = TermView::new(GEOMETRY);
        let top = |t: &mut TermBuffer| -> String {
            view.frame(t, &mut Images::new()).text[0].rows[0]
                .iter()
                .map(|c| c.text.as_str())
                .collect::<String>()
                .trim_end()
                .to_owned()
        };
        assert!(!top(&mut term).contains('['), "at the bottom, nothing");
        term.mouse(&mouse(MouseEventKind::Scroll(ScrollDirection::Up), 0, 0, Modifiers::NONE), false);
        // 20 lines and the prompt line on 6 rows: 15 of history, 3 scrolled.
        let line = top(&mut term);
        assert!(line.ends_with("[3/15]"), "{line:?}");
        assert!(view.frame(&mut term, &mut Images::new()).text[0].rows[0][39].attrs.inverse);
        term.key(&KeyEvent::new(KeyCode::Char('x'), Modifiers::NONE));
        assert!(!top(&mut term).contains('['), "typing returns to the bottom");
    }

    #[test]
    fn the_pane_reports_its_program_exiting() {
        let (pane, _) = run(&["sh", "-c", "exit 0"], |_| {}, |_| false);
        assert!(pane.exited());
    }

    #[test]
    fn keys_translate_to_physical_keys() {
        let t = keys::translate(&KeyEvent::new(KeyCode::Char('A'), Modifiers::SHIFT)).unwrap();
        assert_eq!(
            (t.key, t.consumed, t.text.as_deref(), t.unshifted),
            (key::Key::A, key::Mods::SHIFT, Some("A"), Some('a'))
        );
        let t = keys::translate(&KeyEvent::new(KeyCode::Char('c'), Modifiers::CTRL)).unwrap();
        assert_eq!((t.key, t.mods, t.text), (key::Key::C, key::Mods::CTRL, None));
        assert_eq!(
            keys::translate(&KeyEvent::new(KeyCode::Char('%'), Modifiers::SHIFT)).unwrap().key,
            key::Key::Digit5
        );
        assert_eq!(keys::translate(&KeyEvent::new(KeyCode::F(5), Modifiers::NONE)).unwrap().key, key::Key::F5);
        assert!(keys::translate(&KeyEvent::new(KeyCode::Other(1), Modifiers::NONE)).is_none());
        // Control characters some terminals report as code points (kitty: the
        // Linefeed keysym xdotool types for "\n").
        assert_eq!(keys::translate(&KeyEvent::new(KeyCode::Char('\n'), Modifiers::NONE)).unwrap().key, key::Key::Enter);
        assert_eq!(keys::translate(&KeyEvent::new(KeyCode::Char('\t'), Modifiers::NONE)).unwrap().key, key::Key::Tab);
    }
}
