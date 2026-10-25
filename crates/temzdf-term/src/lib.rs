//! Terminal panes: a program on a pty, its output interpreted by Ghostty's
//! terminal emulation (`libghostty-vt`) and shown as text in a
//! `temzdf-scene` frame. See doc/session-server.md (step 3).
//!
//! The pty is read on a thread that hands the bytes to the caller's [`Sink`];
//! everything else (libghostty's handles are `!Send`) happens on the caller's
//! thread: [`TermPane::feed`] the bytes, then build a [`TermPane::frame`].

pub mod keys;

use std::cell::RefCell;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use libghostty_vt::render::{CellIterator, CursorVisualStyle, Dirty, RowIterator};
use libghostty_vt::screen::CellWide;
use libghostty_vt::style::{StyleColor, Underline as GUnderline};
use libghostty_vt::terminal::{Mode, ScrollViewport};
use libghostty_vt::{RenderState, Terminal, TerminalOptions, focus, key, mouse, paste};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use temzdf_scene::{Attrs, Cell, Color, CursorShape, Effect, Frame, TextGrid, Underline};
use termpix_core::geom::Geometry;
use termpix_core::input::{CoordUnit, KeyEvent, MouseButton, MouseEvent, MouseEventKind, ScrollDirection};

/// What the pty's reader thread reports.
pub enum TermEvent {
    Output(Vec<u8>),
    /// The pty closed: the program (and everything it started) exited.
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

/// Lines of scrollback kept per pane.
const SCROLLBACK: usize = 10_000;
/// Rows the wheel scrolls through history per step.
const WHEEL_ROWS: isize = 3;

pub struct TermPane {
    terminal: Terminal<'static, 'static>,
    render: RenderState<'static>,
    rows_it: RowIterator<'static>,
    cells_it: CellIterator<'static>,
    key_encoder: key::Encoder<'static>,
    mouse_encoder: mouse::Encoder<'static>,
    writer: Rc<RefCell<Box<dyn Write + Send>>>,
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
    geometry: Geometry,
    /// The text of the viewport, updated from libghostty's dirty rows.
    grid: Vec<Vec<Cell>>,
    effects: Rc<RefCell<Vec<Effect>>>,
    /// The view is scrolled back into history.
    scrolled: bool,
    exited: bool,
}

impl TermPane {
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
        let mut reader = pair.master.try_clone_reader().map_err(io::Error::other)?;
        let writer: Rc<RefCell<Box<dyn Write + Send>>> =
            Rc::new(RefCell::new(pair.master.take_writer().map_err(io::Error::other)?));
        std::thread::Builder::new().name("temzdf-pty".into()).spawn(move || {
            let mut buf = vec![0u8; 64 << 10];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => {
                        sink(TermEvent::Exited);
                        return;
                    }
                    Ok(n) => {
                        if !sink(TermEvent::Output(buf[..n].to_vec())) {
                            return;
                        }
                    }
                }
            }
        })?;

        let mut terminal = Terminal::new(TerminalOptions {
            cols: geometry.cols.max(1),
            rows: geometry.rows.max(1),
            max_scrollback: SCROLLBACK,
        })
        .map_err(ghostty_err)?;
        terminal
            .resize(geometry.cols.max(1), geometry.rows.max(1), geometry.cell.width, geometry.cell.height)
            .map_err(ghostty_err)?;
        // Replies to the program's queries (cursor position, device
        // attributes, ...) go back through the pty.
        let w = writer.clone();
        terminal
            .on_pty_write(move |_, data| {
                let _ = w.borrow_mut().write_all(data);
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
            writer,
            master: pair.master,
            child,
            geometry,
            grid: Vec::new(),
            effects,
            scrolled: false,
            exited: false,
        })
    }

    /// Output read from the pty.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.terminal.vt_write(bytes);
    }

    /// The pty closed.
    pub fn set_exited(&mut self) {
        self.exited = true;
        let _ = self.child.try_wait();
    }

    pub fn exited(&self) -> bool {
        self.exited
    }

    fn write(&self, bytes: &[u8]) {
        if !bytes.is_empty() {
            let _ = self.writer.borrow_mut().write_all(bytes);
        }
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
        let _ = self.master.resize(pty_size(geometry));
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

    /// A key typed while this pane has the focus.
    pub fn key(&mut self, k: &KeyEvent) -> bool {
        let redraw = self.scroll_to_bottom();
        let out = keys::encode(&mut self.key_encoder, &self.terminal, k);
        self.write(&out);
        redraw
    }

    fn scroll_to_bottom(&mut self) -> bool {
        if std::mem::take(&mut self.scrolled) {
            self.terminal.scroll_viewport(ScrollViewport::Bottom);
            return true;
        }
        false
    }

    /// Bracketed paste when the program asked for it.
    pub fn paste(&mut self, text: &str) -> bool {
        let redraw = self.scroll_to_bottom();
        let bracketed = self.terminal.mode(Mode::BRACKETED_PASTE).unwrap_or(false);
        let mut data = text.as_bytes().to_vec();
        let mut buf = vec![0u8; data.len() + 16];
        if let Ok(n) = paste::encode(&mut data, bracketed, &mut buf) {
            self.write(&buf[..n]);
        }
        redraw
    }

    /// The pane gained or lost the focus (reported when the program asked).
    pub fn focus(&mut self, gained: bool) {
        if self.terminal.mode(Mode::FOCUS_EVENT).unwrap_or(false) {
            let mut buf = [0u8; 8];
            let event = if gained { focus::Event::Gained } else { focus::Event::Lost };
            if let Ok(n) = event.encode(&mut buf) {
                self.write(&buf[..n]);
            }
        }
    }

    /// A mouse event in pane coordinates. Programs that track the mouse get
    /// it; otherwise the wheel scrolls through history.
    pub fn mouse(&mut self, m: &MouseEvent) -> bool {
        let cell = self.geometry.cell;
        if self.terminal.is_mouse_tracking().unwrap_or(false) {
            let (x, y) = match m.unit {
                CoordUnit::Pixel => (m.x as f32, m.y as f32),
                CoordUnit::Cell => ((m.x as f32 + 0.5) * cell.width as f32, (m.y as f32 + 0.5) * cell.height as f32),
            };
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
                MouseEventKind::Leave => return false,
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
            return false;
        }
        match m.kind {
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

    /// What the pane shows now.
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

impl Drop for TermPane {
    fn drop(&mut self) {
        // Closing the pane ends its program (as a closing terminal window does).
        let _ = self.child.kill();
        let _ = self.child.try_wait();
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
        mut input: impl FnMut(&mut TermPane),
        done: impl Fn(&[String]) -> bool,
    ) -> (TermPane, Vec<String>) {
        let (tx, rx) = mpsc::channel();
        let tx = std::sync::Mutex::new(tx);
        let sink: Sink = Arc::new(move |e| tx.lock().unwrap().send(e).is_ok());
        let spawn = Spawn { argv: Some(argv.iter().map(|s| s.to_string()).collect()), ..Default::default() };
        let mut pane = TermPane::spawn(&spawn, GEOMETRY, sink).unwrap();
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

    fn text(pane: &mut TermPane) -> Vec<String> {
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
