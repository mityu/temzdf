//! A test harness for terminal programs: run one on a pty behind Ghostty's
//! terminal emulation (`libghostty-vt`, kitty graphics included), drive it with
//! keys, and read its screen as text, cells, cursor and image placements.
//! [`Term::inside_tmux`] puts a private tmux between the two, so programs can
//! also be tested inside tmux. See doc/dev/testing.md.
//!
//! The harness answers the queries a program makes at startup (device
//! attributes, XTVERSION, cell size, kitty graphics), so programs that probe
//! the terminal (like temzdf) start as they would in Ghostty.
//!
//! Waiting is by condition ([`Term::wait_for`]), never by fixed sleeps; a
//! failed wait panics with the screen, and the screen and the bytes received
//! are written to `target/tuitest/<test name>.txt`.
//!
//! [`Screen::render`] draws the screen into pixels: cell backgrounds, text
//! and images (see doc/dev/testing.md).

pub mod keys;

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use libghostty_vt::render::{CellIterator, RowIterator};
use libghostty_vt::screen::CellWide;
use libghostty_vt::style::StyleColor;
use libghostty_vt::terminal::{
    ConformanceLevel, DeviceAttributeFeature, DeviceAttributes, DeviceType, PrimaryDeviceAttributes,
    SecondaryDeviceAttributes, SizeReportSize, TertiaryDeviceAttributes,
};
use libghostty_vt::{RenderState, Terminal, TerminalOptions, key};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use termpix_core::geom::Rect;
use termpix_core::raster::{RgbaImage, RgbaRef, blit};
use termpix_ui::{Canvas, Color as UiColor, Text, TextLayout, TextStyle};

/// How long [`Term::wait_for`] waits before failing.
pub const TIMEOUT: Duration = Duration::from_secs(15);

/// The terminal the harness plays.
#[derive(Clone, Debug)]
pub struct Options {
    pub cols: u16,
    pub rows: u16,
    /// Cell size in pixels.
    pub cell: (u32, u32),
    /// The answer to XTVERSION (`CSI > q`).
    pub xtversion: &'static str,
    /// Environment for the program (on top of a cleaned-up copy of ours).
    pub env: Vec<(String, String)>,
    pub cwd: Option<PathBuf>,
}

impl Default for Options {
    fn default() -> Self {
        Self { cols: 100, rows: 30, cell: (10, 20), xtversion: "ghostty(tuitest)", env: Vec::new(), cwd: None }
    }
}

/// A colour as the terminal stores it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Color {
    Default,
    Palette(u8),
    Rgb(u8, u8, u8),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cell {
    /// Empty for blank cells and the right half of wide characters.
    pub text: String,
    /// The right half of a wide character.
    pub spacer: bool,
    pub fg: Color,
    pub bg: Color,
    pub bold: bool,
    pub italic: bool,
    pub inverse: bool,
    /// The background as drawn: `bg` (or the content's own background
    /// colour, as erasing with a colour leaves) resolved through the
    /// palette, the foreground when `inverse`; `None` for the default
    /// background, which images below the backgrounds show through.
    pub bg_rgb: Option<[u8; 3]>,
    /// The foreground as drawn: `fg` resolved through the palette (the
    /// default foreground when unset), the background (or the default
    /// background) when `inverse`.
    pub fg_rgb: [u8; 3],
}

/// An image placed with the kitty graphics protocol.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Placement {
    pub image: u32,
    pub placement: u32,
    /// Top-left cell in the viewport (may be negative when scrolled off).
    pub col: i32,
    pub row: i32,
    /// Cells covered.
    pub cols: u32,
    pub rows: u32,
    /// Size of the image itself in pixels.
    pub width: u32,
    pub height: u32,
    pub z: i32,
    /// The part of the image shown, `(x, y, width, height)` in its pixels.
    pub source: (u32, u32, u32, u32),
    /// The size it is drawn at, in screen pixels.
    pub pixel_width: u32,
    pub pixel_height: u32,
    /// Offset from the top-left cell's corner, in screen pixels.
    pub x_offset: u32,
    pub y_offset: u32,
}

/// Placements below this z go under the cell backgrounds (the kitty
/// graphics protocol's rule, as in Ghostty and kitty).
pub const BELOW_BACKGROUNDS: i32 = i32::MIN / 2;

/// Images' pixels by image ID, as RGBA (shared between screens while an
/// image is unchanged).
#[derive(Clone, Default)]
pub struct ImagePixels(pub BTreeMap<u32, Arc<RgbaImage>>);

impl std::fmt::Debug for ImagePixels {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_map().entries(self.0.iter().map(|(id, img)| (id, (img.width, img.height)))).finish()
    }
}

/// What the terminal shows.
#[derive(Clone, Debug, Default)]
pub struct Screen {
    pub cells: Vec<Vec<Cell>>,
    pub cursor: Option<(u16, u16)>,
    pub images: Vec<Placement>,
    pub title: String,
    /// Cell size in pixels.
    pub cell: (u32, u32),
    /// The default background colour.
    pub background: [u8; 3],
    /// The pixels of the images placed.
    pub pixels: ImagePixels,
}

impl Screen {
    /// Each row as text, wide characters once, trailing blanks removed.
    pub fn lines(&self) -> Vec<String> {
        self.cells
            .iter()
            .map(|row| {
                let s: String = row
                    .iter()
                    .filter(|c| !c.spacer)
                    .map(|c| if c.text.is_empty() { " " } else { c.text.as_str() })
                    .collect();
                s.trim_end().to_owned()
            })
            .collect()
    }

    /// The whole screen as text, one line per row.
    pub fn text(&self) -> String {
        self.lines().join("\n")
    }

    pub fn contains(&self, needle: &str) -> bool {
        self.lines().iter().any(|l| l.contains(needle))
    }

    /// The first row containing `needle`.
    pub fn find(&self, needle: &str) -> Option<usize> {
        self.lines().iter().position(|l| l.contains(needle))
    }

    pub fn cell(&self, col: u16, row: u16) -> &Cell {
        &self.cells[row as usize][col as usize]
    }

    /// The text of a row from `col` on (cells, so wide characters keep their columns).
    /// Cells showing an image through a kitty Unicode placeholder (U+10EEEE,
    /// as inside tmux), as `(col, row)`. Their placements are not in
    /// [`Screen::images`].
    pub fn placeholder_cells(&self) -> Vec<(u16, u16)> {
        let mut out = Vec::new();
        for (y, row) in self.cells.iter().enumerate() {
            for (x, c) in row.iter().enumerate() {
                if c.text.starts_with('\u{10EEEE}') {
                    out.push((x as u16, y as u16));
                }
            }
        }
        out
    }

    pub fn row_from(&self, row: u16, col: u16) -> String {
        let s: String = self.cells[row as usize][col as usize..]
            .iter()
            .filter(|c| !c.spacer)
            .map(|c| if c.text.is_empty() { " " } else { c.text.as_str() })
            .collect();
        s.trim_end().to_owned()
    }

    /// The screen as pixels, as a terminal would draw it but without the
    /// cursor: the default background, then placements below the
    /// backgrounds ([`BELOW_BACKGROUNDS`]), then the cells' own
    /// backgrounds ([`Cell::bg_rgb`]), then placements with a negative z,
    /// then the text, then the other placements, each set in z order (the
    /// kitty graphics protocol's layers, as Ghostty draws them). A placement
    /// shows its source rectangle scaled to its pixel size by nearest
    /// neighbour (simple and exact; terminals that filter scaled images
    /// differ from it only where an image is scaled, and temzdf draws its
    /// images at their own size), placed at its cell plus its offsets,
    /// blended source-over on sRGB values and clipped to the screen.
    /// Images through Unicode placeholders (inside tmux) are not drawn.
    /// The text is drawn as [`draw_text`] says.
    pub fn render(&self) -> RgbaImage {
        let (cw, ch) = self.cell;
        let rows = self.cells.len() as u32;
        let cols = self.cells.iter().map(Vec::len).max().unwrap_or(0) as u32;
        let [r, g, b] = self.background;
        let mut out = RgbaImage::new(cols * cw, rows * ch, [r, g, b, 255]);
        // Placements in `z`'s range, in z order.
        let draw = |out: &mut RgbaImage, z: std::ops::Range<i32>| {
            for p in self.images.iter().filter(|p| z.contains(&p.z)) {
                let Some(img) = self.pixels.0.get(&p.image) else { continue };
                let (sx, sy, sw, sh) = p.source;
                let dst = Rect::new(
                    p.col * cw as i32 + p.x_offset as i32,
                    p.row * ch as i32 + p.y_offset as i32,
                    p.pixel_width,
                    p.pixel_height,
                );
                let src = RgbaRef { width: img.width, height: img.height, data: &img.data };
                blit(out, dst, src, Rect::new(sx as i32, sy as i32, sw, sh));
            }
        };
        draw(&mut out, i32::MIN..BELOW_BACKGROUNDS);
        for (y, row) in self.cells.iter().enumerate() {
            for (x, c) in row.iter().enumerate() {
                if let Some([r, g, b]) = c.bg_rgb {
                    fill(&mut out, x as u32 * cw, y as u32 * ch, cw, ch, [r, g, b, 255]);
                }
            }
        }
        draw(&mut out, BELOW_BACKGROUNDS..0);
        draw_text(&mut out, &self.cells, self.cell);
        draw(&mut out, 0..i32::MAX);
        out
    }

    /// Where [`Screen::render`] draws text: opaque white over each cell
    /// holding a character (both halves of a wide one), transparent
    /// elsewhere. Pixels under it depend on the fonts, for comparisons
    /// that want to leave them out.
    pub fn text_mask(&self) -> RgbaImage {
        let (cw, ch) = self.cell;
        let rows = self.cells.len() as u32;
        let cols = self.cells.iter().map(Vec::len).max().unwrap_or(0) as u32;
        let mut out = RgbaImage::new(cols * cw, rows * ch, [0, 0, 0, 0]);
        for (y, row) in self.cells.iter().enumerate() {
            for (x, c) in row.iter().enumerate() {
                let wide_tail = c.spacer && x > 0 && !row[x - 1].text.trim().is_empty();
                if !c.text.trim().is_empty() || wide_tail {
                    fill(&mut out, x as u32 * cw, y as u32 * ch, cw, ch, [255, 255, 255, 255]);
                }
            }
        }
        out
    }

    /// [`Screen::render`] and [`Screen::text_mask`], written to
    /// `target/tuitest/NAME.png` and `NAME.mask.png` for people to look at.
    /// Returns the rendering.
    pub fn save_render(&self, name: &str) -> RgbaImage {
        let dir = out_dir();
        let _ = std::fs::create_dir_all(&dir);
        let img = self.render();
        img.save_png(&dir.join(format!("{name}.png"))).expect("write the rendering");
        self.text_mask().save_png(&dir.join(format!("{name}.mask.png"))).expect("write the text mask");
        img
    }
}

/// A cell size and the font size picked for it.
type PickedSize = Option<((u32, u32), f32)>;

thread_local! {
    /// The fonts and layout scratch space, and the font size picked for a
    /// cell size (loading the fonts takes a while; once per test thread).
    static TEXT: RefCell<(Text, PickedSize)> = RefCell::new((Text::new(), None));
}

/// Draw each cell's text over `out` (cells of `cell` pixels) in its
/// [`Cell::fg_rgb`], bold as [`Cell::bold`] says; italic is drawn upright
/// (termpix-ui has no italic). Every character is laid out on its own with
/// termpix-ui (parley, fonts from fontconfig: family `monospace`, which
/// the tests point at JetBrains Mono with `TEMZDF_TEST_FONTCONFIG`), so it
/// stays in its cell whatever font it falls back to.
///
/// The font size is the largest at which both a character's advance fits
/// the cell's width and the line height (the font's ascent plus descent
/// plus line gap, as parley lays out one line) fits its height, measured
/// on `0` at size 100 and scaled. Each character is centred in its cell
/// (in both cells for a wide one, which also centres fonts' glyphs that
/// are narrower or wider than two cells, like emoji): horizontally on its
/// advance, vertically on the line, its top rounded to a whole pixel so
/// that every row's baseline sits at the same pixel in its cell. Box
/// drawing characters are the font's glyphs too (terminals draw their own,
/// so lines may not join across cells here). No cursor.
pub fn draw_text(out: &mut RgbaImage, cells: &[Vec<Cell>], cell: (u32, u32)) {
    let (cw, ch) = cell;
    let has_text = |c: &Cell| !c.spacer && !c.text.trim().is_empty();
    if cw == 0 || ch == 0 || !cells.iter().flatten().any(has_text) {
        return;
    }
    let style = |size: f32, bold: bool, [r, g, b]: [u8; 3]| TextStyle {
        size,
        color: UiColor::rgb(r, g, b),
        bold,
        family: "monospace",
    };
    let mut canvas = Canvas::new(out.width, out.height);
    TEXT.with_borrow_mut(|(text, picked)| {
        let size = match *picked {
            Some((c, size)) if c == cell => size,
            _ => {
                let probe = text.layout("0", &style(100.0, false, [0; 3]), None);
                let size = (cw as f32 * 100.0 / probe.width()).min(ch as f32 * 100.0 / probe.height());
                *picked = Some((cell, size));
                size
            }
        };
        let mut layouts: HashMap<(&str, bool, [u8; 3]), TextLayout> = HashMap::new();
        for (y, row) in cells.iter().enumerate() {
            for (x, c) in row.iter().enumerate() {
                if !has_text(c) {
                    continue;
                }
                let span = if row.get(x + 1).is_some_and(|n| n.spacer) { 2 } else { 1 };
                let layout = layouts
                    .entry((c.text.as_str(), c.bold, c.fg_rgb))
                    .or_insert_with(|| text.layout(&c.text, &style(size, c.bold, c.fg_rgb), None));
                let left = (x as u32 * cw) as f32 + ((span * cw) as f32 - layout.width()) / 2.0;
                let top = (y as u32 * ch) as f32 + ((ch as f32 - layout.height()) / 2.0).round();
                canvas.text(layout, left, top);
            }
        }
    });
    let (w, h) = (canvas.width(), canvas.height());
    let pixels = canvas.finish();
    let src = RgbaRef { width: w, height: h, data: &pixels };
    blit(out, Rect::new(0, 0, w, h), src, Rect::new(0, 0, w, h));
}

/// Fill a rectangle of `img` (clipped to it) with `color`.
fn fill(img: &mut RgbaImage, x: u32, y: u32, w: u32, h: u32, color: [u8; 4]) {
    let x1 = (x + w).min(img.width);
    for yy in y..(y + h).min(img.height) {
        for xx in x..x1 {
            let i = (yy as usize * img.width as usize + xx as usize) * 4;
            img.data[i..i + 4].copy_from_slice(&color);
        }
    }
}

/// Where failure dumps and renderings go: `target/tuitest/`.
pub fn out_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/tuitest")
}

/// An image as the terminal stores it, as RGBA. `None` for formats the
/// harness does not take: PNG never gets stored (the harness sets no PNG
/// decoder, so the terminal refuses it; termpix sends raw RGB or RGBA).
fn to_rgba(
    format: libghostty_vt::kitty::graphics::ImageFormat,
    compression: libghostty_vt::kitty::graphics::Compression,
    data: &[u8],
    width: u32,
    height: u32,
) -> Option<RgbaImage> {
    use libghostty_vt::kitty::graphics::{Compression, ImageFormat};
    let inflated;
    let data = match compression {
        Compression::None => data,
        Compression::ZlibDeflate => {
            let mut out = Vec::new();
            flate2::read::ZlibDecoder::new(data).read_to_end(&mut out).ok()?;
            inflated = out;
            &inflated
        }
        _ => return None,
    };
    let n = width as usize * height as usize;
    let rgba: Vec<u8> = match format {
        ImageFormat::Rgba => data.get(..n * 4)?.to_vec(),
        ImageFormat::Rgb => {
            data.get(..n * 3)?.as_chunks::<3>().0.iter().flat_map(|&[r, g, b]| [r, g, b, 255]).collect()
        }
        ImageFormat::GrayAlpha => {
            data.get(..n * 2)?.as_chunks::<2>().0.iter().flat_map(|&[v, a]| [v, v, v, a]).collect()
        }
        ImageFormat::Gray => data.get(..n)?.iter().flat_map(|&v| [v, v, v, 255]).collect(),
        _ => return None,
    };
    RgbaImage::from_raw(width, height, rgba)
}

/// A directory removed when dropped (for sockets, logs and state of one test).
pub struct TempDir(PathBuf);

impl TempDir {
    pub fn new(label: &str) -> Self {
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        // Short: sockets live inside (Unix socket paths are limited to ~100
        // bytes, and macOS's per-user temporary directory is long).
        let base = if Path::new("/tmp").is_dir() { PathBuf::from("/tmp") } else { std::env::temp_dir() };
        let dir = base.join(format!("tt-{}-{n}-{label}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temporary directory");
        Self(dir)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A private tmux server (removed on drop, with its configuration file).
struct Tmux {
    socket: String,
    conf: PathBuf,
}

impl Tmux {
    fn cmd(&self) -> std::process::Command {
        let mut c = std::process::Command::new("tmux");
        c.args(["-L", &self.socket]);
        c
    }
}

impl Drop for Tmux {
    fn drop(&mut self) {
        // kill-server leaves the socket file behind: remove it too.
        let socket = self.cmd().args(["display-message", "-p", "#{socket_path}"]).output().ok();
        let _ = self.cmd().arg("kill-server").stderr(std::process::Stdio::null()).status();
        if let Some(out) = socket.filter(|o| o.status.success()) {
            let path = String::from_utf8_lossy(&out.stdout).trim().to_owned();
            if !path.is_empty() {
                let _ = std::fs::remove_file(path);
            }
        }
        let _ = std::fs::remove_file(&self.conf);
    }
}

/// Whether `tmux` can be run (tests inside tmux are skipped without it).
pub fn have_tmux() -> bool {
    std::process::Command::new("tmux").arg("-V").stdout(std::process::Stdio::null()).status().is_ok_and(|s| s.success())
}

#[derive(Clone, Copy)]
enum MouseStep {
    Move,
    Press,
    Drag,
    Release,
    WheelUp,
    WheelDown,
}

pub struct Term {
    terminal: Terminal<'static, 'static>,
    render: RenderState<'static>,
    rows_it: RowIterator<'static>,
    cells_it: CellIterator<'static>,
    placements: libghostty_vt::kitty::graphics::PlacementIterator<'static>,
    encoder: key::Encoder<'static>,
    writer: Rc<RefCell<Box<dyn Write + Send>>>,
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
    output: Receiver<Vec<u8>>,
    /// The pty closed.
    eof: bool,
    /// Everything received (for the failure dump).
    received: Vec<u8>,
    size: Rc<RefCell<(u16, u16, u32, u32)>>,
    tmux: Option<Tmux>,
    /// Text the program copied (OSC 52), oldest first.
    copied: Rc<RefCell<Vec<String>>>,
    /// Images converted to RGBA, by ID, with the generation converted.
    pixels: HashMap<u32, (u64, Arc<RgbaImage>)>,
}

impl Term {
    /// Run `argv` on a new pty.
    pub fn spawn(argv: &[&str], opts: &Options) -> Self {
        Self::start(argv, opts, None)
    }

    /// Run `argv` inside a private tmux server (passthrough on, no status
    /// line, no prefix key: every key reaches `argv`), shown in this
    /// terminal.
    pub fn inside_tmux(argv: &[&str], opts: &Options) -> Self {
        static N: AtomicU32 = AtomicU32::new(0);
        let socket = format!("tuitest-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed));
        let conf = std::env::temp_dir().join(format!("{socket}.conf"));
        // A new file only: the name is predictable in a shared directory,
        // and an existing one (or a link planted there) is not ours.
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&conf)
            .and_then(|mut f| {
                std::io::Write::write_all(
                    &mut f,
                    b"set -g allow-passthrough on\nset -g status off\nset -sg escape-time 0\nset -g default-terminal tmux-256color\n\
                      set -g prefix None\nset -g prefix2 None\nunbind C-b\n",
                )
            })
            .expect("tmux configuration");
        let mut full: Vec<String> = vec!["tmux".into(), "-L".into(), socket.clone(), "-f".into()];
        full.push(conf.display().to_string());
        let tmux = Tmux { socket, conf };
        full.push("new-session".into());
        // tmux passes the command to a shell: quote the words.
        full.push(argv.iter().map(|a| shell_quote(a)).collect::<Vec<_>>().join(" "));
        let words: Vec<&str> = full.iter().map(String::as_str).collect();
        Self::start(&words, opts, Some(tmux))
    }

    fn start(argv: &[&str], opts: &Options, tmux: Option<Tmux>) -> Self {
        let size = PtySize {
            rows: opts.rows,
            cols: opts.cols,
            pixel_width: opts.cols * opts.cell.0 as u16,
            pixel_height: opts.rows * opts.cell.1 as u16,
        };
        let pair = native_pty_system().openpty(size).expect("pty");
        let mut cmd = CommandBuilder::new(argv[0]);
        cmd.args(&argv[1..]);
        if let Some(dir) = &opts.cwd {
            cmd.cwd(dir);
        } else if let Ok(dir) = std::env::current_dir() {
            cmd.cwd(dir);
        }
        // Whatever terminal or multiplexer runs the tests must not leak in.
        for var in [
            "TMUX",
            "TMUX_PANE",
            "ZELLIJ",
            "ZELLIJ_SESSION_NAME",
            "ZELLIJ_PANE_ID",
            "KITTY_WINDOW_ID",
            "KITTY_PID",
            "WEZTERM_PANE",
            "TERM_PROGRAM",
            "TERM_PROGRAM_VERSION",
            "TERMPIX_BACKEND",
            "TERMPIX_MEDIUM",
            "TERMPIX_TRACE",
            "SSH_CONNECTION",
            "SSH_CLIENT",
            "SSH_TTY",
            "TEMZDF_SESSION_NAME",
        ] {
            cmd.env_remove(var);
        }
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        for (k, v) in &opts.env {
            cmd.env(k, v);
        }
        let child = pair.slave.spawn_command(cmd).unwrap_or_else(|e| panic!("cannot run {argv:?}: {e}"));
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader().expect("pty reader");
        let writer: Rc<RefCell<Box<dyn Write + Send>>> =
            Rc::new(RefCell::new(pair.master.take_writer().expect("pty writer")));
        let (tx, output) = mpsc::channel();
        std::thread::spawn(move || {
            let mut buf = vec![0u8; 1 << 16];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
        });

        let mut terminal = Terminal::new(TerminalOptions { cols: opts.cols, rows: opts.rows, max_scrollback: 1000 })
            .expect("terminal");
        terminal.resize(opts.cols, opts.rows, opts.cell.0, opts.cell.1).expect("resize");
        terminal.set_kitty_image_storage_limit(1 << 30).expect("graphics");
        let _ = terminal.set_kitty_image_from_shared_mem_allowed(true);
        let _ = terminal.set_kitty_image_from_temp_file_allowed(true);
        let _ = terminal.set_kitty_image_from_file_allowed(true);
        let w = writer.clone();
        terminal
            .on_pty_write(move |_, data| {
                let _ = w.borrow_mut().write_all(data);
            })
            .expect("callback");
        let copied: Rc<RefCell<Vec<String>>> = Rc::default();
        let c = copied.clone();
        terminal
            .on_clipboard_write(move |_, write| {
                for content in write.contents() {
                    if content.mime.starts_with("text/plain") {
                        c.borrow_mut().push(content.data.to_owned());
                        break;
                    }
                }
                Ok(())
            })
            .expect("callback");
        let xtversion = opts.xtversion;
        terminal.on_xtversion(move |_| Some(xtversion)).expect("callback");
        let size = Rc::new(RefCell::new((opts.rows, opts.cols, opts.cell.0, opts.cell.1)));
        let s = size.clone();
        terminal
            .on_size(move |_| {
                let (rows, columns, cell_width, cell_height) = *s.borrow();
                Some(SizeReportSize { rows, columns, cell_width, cell_height })
            })
            .expect("callback");
        terminal
            .on_device_attributes(|_| {
                Some(DeviceAttributes {
                    primary: PrimaryDeviceAttributes::new(
                        ConformanceLevel::LEVEL_2,
                        &[DeviceAttributeFeature::COLUMNS_132, DeviceAttributeFeature::SELECTIVE_ERASE],
                    ),
                    secondary: SecondaryDeviceAttributes {
                        device_type: DeviceType::VT220,
                        firmware_version: 1,
                        rom_cartridge: 0,
                    },
                    tertiary: TertiaryDeviceAttributes::default(),
                })
            })
            .expect("callback");
        Self {
            terminal,
            render: RenderState::new().expect("render state"),
            rows_it: RowIterator::new().expect("rows"),
            cells_it: CellIterator::new().expect("cells"),
            placements: libghostty_vt::kitty::graphics::PlacementIterator::new().expect("placements"),
            encoder: key::Encoder::new().expect("encoder"),
            writer,
            master: pair.master,
            child,
            output,
            eof: false,
            received: Vec::new(),
            size,
            tmux,
            copied,
            pixels: HashMap::new(),
        }
    }

    /// Interpret output that arrived within `wait`. Returns whether any did.
    pub fn pump(&mut self, wait: Duration) -> bool {
        let mut got = false;
        let deadline = Instant::now() + wait;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.output.recv_timeout(left) {
                Ok(bytes) => {
                    self.received.extend_from_slice(&bytes);
                    self.terminal.vt_write(&bytes);
                    got = true;
                    // Take what else is there right away.
                    while let Ok(more) = self.output.try_recv() {
                        self.received.extend_from_slice(&more);
                        self.terminal.vt_write(&more);
                    }
                }
                Err(RecvTimeoutError::Timeout) => return got,
                Err(RecvTimeoutError::Disconnected) => {
                    self.eof = true;
                    return got;
                }
            }
            if Instant::now() >= deadline {
                return got;
            }
        }
    }

    /// Every frame the program draws until one satisfies `done` (that one
    /// last): the screen at the end of each synchronized update (`CSI ?
    /// 2026 l`), to catch states that last a frame only (a popup gone for
    /// a moment). Panics (with a dump) after [`TIMEOUT`]. Output after the
    /// last frame's end is interpreted too, without a screen of its own.
    pub fn frames_until(&mut self, what: &str, done: impl Fn(&Screen) -> bool) -> Vec<Screen> {
        const END: &[u8] = b"\x1b[?2026l";
        let mut out = Vec::new();
        let mut pending: Vec<u8> = Vec::new();
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let why = match self.output.recv_timeout(left) {
                Ok(bytes) => {
                    self.received.extend_from_slice(&bytes);
                    pending.extend_from_slice(&bytes);
                    while let Some(at) = pending.windows(END.len()).position(|w| w == END) {
                        let frame: Vec<u8> = pending.drain(..at + END.len()).collect();
                        self.terminal.vt_write(&frame);
                        let screen = self.snapshot();
                        let last = done(&screen);
                        out.push(screen);
                        if last {
                            self.terminal.vt_write(&pending);
                            return out;
                        }
                    }
                    if Instant::now() < deadline {
                        continue;
                    }
                    "timed out"
                }
                Err(RecvTimeoutError::Timeout) => "timed out",
                Err(RecvTimeoutError::Disconnected) => {
                    self.eof = true;
                    "the program exited"
                }
            };
            self.terminal.vt_write(&pending);
            let screen = self.snapshot();
            self.fail(&format!("waiting for a frame with {what}: {why} ({} frames)", out.len()), &screen);
        }
    }

    /// What the terminal shows now (after interpreting what has arrived).
    pub fn screen(&mut self) -> Screen {
        self.pump(Duration::ZERO);
        self.snapshot()
    }

    /// The screen as interpreted so far.
    fn snapshot(&mut self) -> Screen {
        let mut cells = Vec::new();
        let mut cursor = None;
        let mut background = [0, 0, 0];
        if let Ok(snapshot) = self.render.update(&self.terminal) {
            let colors = snapshot.colors().ok();
            if let Some(c) = &colors {
                background = [c.background.r, c.background.g, c.background.b];
            }
            let foreground =
                colors.as_ref().map_or([255, 255, 255], |c| [c.foreground.r, c.foreground.g, c.foreground.b]);
            if let Ok(mut rows) = self.rows_it.update(&snapshot) {
                while let Some(row) = rows.next() {
                    let mut line = Vec::new();
                    if let Ok(mut it) = self.cells_it.update(row) {
                        while let Some(c) = it.next() {
                            let spacer = matches!(c.raw_cell().and_then(|r| r.wide()), Ok(CellWide::SpacerTail));
                            let text = if spacer {
                                String::new()
                            } else {
                                c.graphemes().map(|g| g.into_iter().collect()).unwrap_or_default()
                            };
                            let (fg, bg, bold, italic, inverse) = match c.style() {
                                Ok(s) => (color(s.fg_color), color(s.bg_color), s.bold, s.italic, s.inverse),
                                Err(_) => (Color::Default, Color::Default, false, false, false),
                            };
                            let rgb = |c: Option<libghostty_vt::style::RgbColor>| c.map(|c| [c.r, c.g, c.b]);
                            let fg_rgb = rgb(c.fg_color().ok().flatten()).unwrap_or(foreground);
                            let bg_own = rgb(c.bg_color().ok().flatten());
                            let (bg_rgb, fg_rgb) =
                                if inverse { (Some(fg_rgb), bg_own.unwrap_or(background)) } else { (bg_own, fg_rgb) };
                            line.push(Cell { text, spacer, fg, bg, bold, italic, inverse, bg_rgb, fg_rgb });
                        }
                    }
                    cells.push(line);
                }
            }
            if snapshot.cursor_visible().unwrap_or(false)
                && let Ok(Some(c)) = snapshot.cursor_viewport()
            {
                cursor = Some((c.x, c.y));
            }
        }
        let (images, pixels) = self.images();
        let title = self.terminal.title().map(str::to_owned).unwrap_or_default();
        let (_, _, cw, ch) = *self.size.borrow();
        Screen { cells, cursor, images, title, cell: (cw, ch), background, pixels }
    }

    /// The placements shown, and their images' pixels.
    fn images(&mut self) -> (Vec<Placement>, ImagePixels) {
        let mut out = Vec::new();
        let mut pixels = ImagePixels::default();
        let Ok(graphics) = self.terminal.kitty_graphics() else { return (out, pixels) };
        let Ok(mut it) = self.placements.update(&graphics) else { return (out, pixels) };
        while let Some(p) = it.next() {
            let Ok(id) = p.image_id() else { continue };
            let Some(image) = graphics.image(id) else { continue };
            let pos = p.viewport_pos(&image, &self.terminal).ok().flatten();
            let grid = p.grid_size(&image, &self.terminal).unwrap_or_default();
            let Some(pos) = pos else { continue };
            let size = p.pixel_size(&image, &self.terminal).unwrap_or_default();
            let src = p.source_rect(&image).unwrap_or_default();
            if let std::collections::btree_map::Entry::Vacant(slot) = pixels.0.entry(id) {
                let generation = image.generation().unwrap_or(0);
                let cached = self.pixels.get(&id).filter(|(g, _)| *g == generation).map(|(_, img)| img.clone());
                let converted = cached.or_else(|| {
                    let img = to_rgba(
                        image.format().ok()?,
                        image.compression().ok()?,
                        image.data().ok()?,
                        image.width().ok()?,
                        image.height().ok()?,
                    )?;
                    let img = Arc::new(img);
                    self.pixels.insert(id, (generation, img.clone()));
                    Some(img)
                });
                if let Some(img) = converted {
                    slot.insert(img);
                }
            }
            out.push(Placement {
                image: id,
                placement: p.placement_id().unwrap_or(0),
                col: pos.col,
                row: pos.row,
                cols: grid.cols,
                rows: grid.rows,
                width: image.width().unwrap_or(0),
                height: image.height().unwrap_or(0),
                z: p.z().unwrap_or(0),
                source: (src.x, src.y, src.width, src.height),
                pixel_width: size.width,
                pixel_height: size.height,
                x_offset: p.x_offset().unwrap_or(0),
                y_offset: p.y_offset().unwrap_or(0),
            });
        }
        // Images no longer placed leave the cache.
        self.pixels.retain(|id, _| pixels.0.contains_key(id));
        out.sort_by_key(|p| (p.z, p.row, p.col));
        (out, pixels)
    }

    /// Wait until `done` holds for the screen; panics (with a dump) after
    /// [`TIMEOUT`]. `what` names the condition in the failure message.
    pub fn wait_for(&mut self, what: &str, done: impl Fn(&Screen) -> bool) -> Screen {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let screen = self.screen();
            if done(&screen) {
                return screen;
            }
            if Instant::now() >= deadline || (self.eof && !self.pump(Duration::ZERO)) {
                let why = if self.eof { "the program exited" } else { "timed out" };
                self.fail(&format!("waiting for {what}: {why}"), &screen);
            }
            self.pump(Duration::from_millis(50));
        }
    }

    /// [`Self::wait_for`] on several terminals at once: until `done` holds
    /// for each. A `Term` answers its program's queries (the capability
    /// probe at startup among them) only while it is pumped, so waiting on
    /// one program while another starts up can leave the other unanswered
    /// past its probe's timeout; this pumps them all in turn.
    pub fn wait_for_all(terms: &mut [&mut Term], what: &str, done: impl Fn(&Screen) -> bool) -> Vec<Screen> {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let screens: Vec<Screen> = terms.iter_mut().map(|t| t.screen()).collect();
            if screens.iter().all(&done) {
                return screens;
            }
            for (t, screen) in terms.iter_mut().zip(&screens) {
                if done(screen) {
                    continue;
                }
                if Instant::now() >= deadline || (t.eof && !t.pump(Duration::ZERO)) {
                    let why = if t.eof { "the program exited" } else { "timed out" };
                    t.fail(&format!("waiting for {what}: {why}"), screen);
                }
            }
            for t in terms.iter_mut() {
                t.pump(Duration::from_millis(10));
            }
        }
    }

    /// Wait until `needle` is on the screen.
    pub fn wait_text(&mut self, needle: &str) -> Screen {
        self.wait_for(&format!("{needle:?}"), |s| s.contains(needle))
    }

    /// Wait until nothing arrives for `quiet` (e.g. after the last frame).
    /// A quiet spell does not prove the program is done (a loaded machine
    /// can pause it longer): wait for what is expected with [`Self::wait_for`]
    /// and use this only to let trailing output pass.
    pub fn settle(&mut self, quiet: Duration) -> Screen {
        let deadline = Instant::now() + TIMEOUT;
        while self.pump(quiet) && Instant::now() < deadline {}
        self.screen()
    }

    /// Fail the test with the screen, and leave the screen and the raw output
    /// in `target/tuitest/`.
    pub fn fail(&self, message: &str, screen: &Screen) -> ! {
        let name = std::thread::current().name().unwrap_or("tuitest").replace("::", "-");
        let dir = out_dir();
        let _ = std::fs::create_dir_all(&dir);
        let dump = format!(
            "{message}\n--- screen ({} images, cursor {:?}) ---\n{}\n---\n",
            screen.images.len(),
            screen.cursor,
            screen.text()
        );
        let _ = std::fs::write(dir.join(format!("{name}.txt")), &dump);
        let _ = std::fs::write(dir.join(format!("{name}.out")), &self.received);
        panic!("{dump}(also in target/tuitest/{name}.txt; raw output in {name}.out)");
    }

    /// Send raw bytes as if typed.
    pub fn write(&mut self, bytes: &[u8]) {
        let mut w = self.writer.borrow_mut();
        w.write_all(bytes).expect("write to the pty");
        let _ = w.flush();
    }

    /// Press keys, in the notation of [`keys::parse`] (`"C-h"`, `"Enter"`,
    /// `"M-x"`, `"%"`), encoded the way the program asked for (legacy or the
    /// kitty keyboard protocol).
    pub fn key(&mut self, spec: &str) {
        self.pump(Duration::ZERO);
        let press = keys::parse(spec).unwrap_or_else(|e| panic!("{e}"));
        self.encoder.set_options_from_terminal(&self.terminal);
        let mut bytes = Vec::new();
        self.encoder.encode_to_vec(&press.event(), &mut bytes).expect("encode");
        assert!(!bytes.is_empty(), "no encoding for {spec:?}");
        self.write(&bytes);
    }

    /// Click the left button at the centre of a cell, encoded as the program
    /// asked (SGR, SGR-pixels, ...); nothing is sent if it tracks no mouse.
    pub fn click(&mut self, col: u16, row: u16) {
        self.mouse(&[(MouseStep::Press, col, row, 0.5), (MouseStep::Release, col, row, 0.5)]);
    }

    /// Drag with the left button from one cell to another (ending past the
    /// middle of the last cell, so a selection includes it).
    pub fn drag(&mut self, from: (u16, u16), to: (u16, u16)) {
        self.mouse(&[
            (MouseStep::Press, from.0, from.1, 0.25),
            (MouseStep::Drag, to.0, to.1, 0.75),
            (MouseStep::Release, to.0, to.1, 0.75),
        ]);
    }

    /// Move the pointer, no button held, to the centre of a cell (sent only
    /// if the program tracks any motion, mode 1003).
    pub fn hover(&mut self, col: u16, row: u16) {
        self.mouse(&[(MouseStep::Move, col, row, 0.5)]);
    }

    /// Turn the mouse wheel by one notch over a cell.
    pub fn wheel(&mut self, col: u16, row: u16, down: bool) {
        self.mouse(&[(if down { MouseStep::WheelDown } else { MouseStep::WheelUp }, col, row, 0.5)]);
    }

    /// Text the program wrote to the clipboard (OSC 52) since the start.
    pub fn copied(&mut self) -> Vec<String> {
        self.pump(Duration::ZERO);
        self.copied.borrow().clone()
    }

    /// Left-button and wheel events at cells (`x` the fraction of the cell's width).
    fn mouse(&mut self, steps: &[(MouseStep, u16, u16, f32)]) {
        use libghostty_vt::mouse;
        self.pump(Duration::ZERO);
        let (rows, cols, cw, ch) = *self.size.borrow();
        let mut encoder = mouse::Encoder::new().expect("mouse encoder");
        encoder.set_options_from_terminal(&self.terminal);
        encoder.set_size(mouse::EncoderSize {
            screen_width: cols as u32 * cw,
            screen_height: rows as u32 * ch,
            cell_width: cw,
            cell_height: ch,
            padding_top: 0,
            padding_bottom: 0,
            padding_right: 0,
            padding_left: 0,
        });
        for &(step, col, row, frac) in steps {
            let position = mouse::Position { x: (col as f32 + frac) * cw as f32, y: (row as f32 + 0.5) * ch as f32 };
            let (action, pressed, button) = match step {
                MouseStep::Move => (mouse::Action::Motion, false, None),
                MouseStep::Press => (mouse::Action::Press, true, Some(mouse::Button::Left)),
                MouseStep::Drag => (mouse::Action::Motion, true, Some(mouse::Button::Left)),
                MouseStep::Release => (mouse::Action::Release, false, Some(mouse::Button::Left)),
                MouseStep::WheelUp => (mouse::Action::Press, false, Some(mouse::Button::Four)),
                MouseStep::WheelDown => (mouse::Action::Press, false, Some(mouse::Button::Five)),
            };
            let mut event = mouse::Event::new().expect("mouse event");
            event.set_action(action).set_button(button).set_position(position);
            encoder.set_any_button_pressed(pressed);
            let mut bytes = Vec::new();
            if encoder.encode_to_vec(&event, &mut bytes).is_ok() && !bytes.is_empty() {
                self.write(&bytes);
            }
        }
    }

    pub fn keys(&mut self, specs: &[&str]) {
        for s in specs {
            self.key(s);
        }
    }

    /// Type text key by key, then Enter if it ends with a newline.
    pub fn type_text(&mut self, text: &str) {
        for c in text.chars() {
            match c {
                '\n' => self.key("Enter"),
                ' ' => self.key("Space"),
                c => self.key(&c.to_string()),
            }
        }
    }

    /// The window gained or lost the focus: a focus report (CSI I / CSI O),
    /// when the program asked for them (mode 1004), as a terminal sends.
    pub fn focus(&mut self, gained: bool) {
        self.pump(Duration::ZERO);
        if self.terminal.mode(libghostty_vt::terminal::Mode::FOCUS_EVENT).unwrap_or(false) {
            self.write(if gained { b"\x1b[I" } else { b"\x1b[O" });
        }
    }

    /// Paste, bracketed when the program asked for it.
    pub fn paste(&mut self, text: &str) {
        self.pump(Duration::ZERO);
        let bracketed = self.terminal.mode(libghostty_vt::terminal::Mode::BRACKETED_PASTE).unwrap_or(false);
        let mut data = text.as_bytes().to_vec();
        let mut buf = vec![0u8; data.len() + 16];
        let n = libghostty_vt::paste::encode(&mut data, bracketed, &mut buf).expect("paste");
        self.write(&buf[..n]);
    }

    /// Resize the window (the pty reports it to the program).
    pub fn resize(&mut self, cols: u16, rows: u16) {
        let (_, _, cw, ch) = *self.size.borrow();
        *self.size.borrow_mut() = (rows, cols, cw, ch);
        self.terminal.resize(cols, rows, cw, ch).expect("resize");
        self.master
            .resize(PtySize { rows, cols, pixel_width: cols * cw as u16, pixel_height: rows * ch as u16 })
            .expect("pty resize");
    }

    /// Wait for the program to exit; its exit code.
    pub fn wait_exit(&mut self) -> u32 {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            self.pump(Duration::from_millis(20));
            if let Ok(Some(status)) = self.child.try_wait() {
                self.pump(Duration::from_millis(50));
                return status.exit_code();
            }
            if Instant::now() >= deadline {
                let screen = self.screen();
                self.fail("waiting for the program to exit", &screen);
            }
        }
    }

    /// The pane as tmux sees it (`capture-pane -p`), for [`Term::inside_tmux`].
    pub fn capture_pane(&self) -> String {
        let tmux = self.tmux.as_ref().expect("not inside tmux");
        let out = tmux.cmd().args(["capture-pane", "-p"]).output().expect("tmux capture-pane");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }
}

impl Drop for Term {
    fn drop(&mut self) {
        // Programs killed (or a test ended) before the terminal read an
        // image leave its shared memory behind: note every process of the
        // tree first, then remove what they made once they are gone.
        let mut roots: Vec<u32> = self.child.process_id().into_iter().collect();
        if let Some(tmux) = &self.tmux {
            // The private tmux server is not below the child (the tmux client).
            let out = tmux.cmd().args(["display-message", "-p", "#{pid}"]).output().ok();
            roots.extend(out.and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse::<u32>().ok()));
        }
        let pids = process_tree(&roots);
        let _ = self.child.kill();
        let _ = self.child.try_wait();
        // Kills the tmux server (and the programs in it) now.
        self.tmux = None;
        remove_image_transfers(&pids);
    }
}

/// `roots` and all their descendants (Linux; just `roots` elsewhere).
fn process_tree(roots: &[u32]) -> Vec<u32> {
    let mut all: Vec<u32> = roots.to_vec();
    let Ok(dir) = std::fs::read_dir("/proc") else { return all };
    let mut parent_of: Vec<(u32, u32)> = Vec::new();
    for e in dir.flatten() {
        let Some(pid) = e.file_name().to_str().and_then(|n| n.parse::<u32>().ok()) else { continue };
        let Ok(stat) = std::fs::read_to_string(e.path().join("stat")) else { continue };
        // "pid (comm) S ppid ...": comm may hold spaces and parentheses.
        let ppid = stat.rsplit_once(')').and_then(|(_, rest)| rest.split_whitespace().nth(1)?.parse::<u32>().ok());
        if let Some(ppid) = ppid {
            parent_of.push((pid, ppid));
        }
    }
    let mut i = 0;
    while i < all.len() {
        let p = all[i];
        for &(pid, ppid) in &parent_of {
            if ppid == p && !all.contains(&pid) {
                all.push(pid);
            }
        }
        i += 1;
    }
    all
}

/// Remove the shared memory `/dev/shm/termpix-PID-*` of `pids` once they are
/// gone (a terminal unlinks it when it reads the image; killed ones do not).
fn remove_image_transfers(pids: &[u32]) {
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while pids.iter().any(|p| Path::new(&format!("/proc/{p}")).exists()) && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    let Ok(dir) = std::fs::read_dir("/dev/shm") else { return };
    let prefixes: Vec<String> = pids.iter().map(|p| format!("termpix-{p}-")).collect();
    for e in dir.flatten() {
        if let Some(name) = e.file_name().to_str()
            && prefixes.iter().any(|p| name.starts_with(p.as_str()))
        {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

fn color(c: StyleColor) -> Color {
    match c {
        StyleColor::None => Color::Default,
        StyleColor::Palette(i) => Color::Palette(i.0),
        StyleColor::Rgb(c) => Color::Rgb(c.r, c.g, c.b),
    }
}

/// `s` quoted for `sh`.
pub fn shell_quote(s: &str) -> String {
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_./=:,+@%".contains(c)) {
        s.to_owned()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The command line of `temzdf-testhelper` (a program for tests to run
    /// instead of `sh -c`) with these commands.
    fn helper<'a>(commands: &[&'a str]) -> Vec<&'a str> {
        std::iter::once(temzdf_testhelper::path()).chain(commands.iter().copied()).collect()
    }

    #[test]
    fn shows_output_colours_and_wide_characters() {
        let mut t =
            Term::spawn(&helper(&["print", "a \\e[31mred\\e[0m 数式\\n", "sleep", "5000"]), &Options::default());
        let s = t.wait_text("red");
        assert_eq!(s.lines()[0], "a red 数式");
        assert_eq!(s.cell(2, 0).fg, Color::Palette(1));
        assert!(s.cell(7, 0).spacer, "数 is wide");
        assert_eq!(s.cell(8, 0).text, "式");
    }

    #[test]
    fn keys_follow_the_programs_keyboard_mode() {
        // What arrives: legacy C-h is 0x08 (a line of its own, ended by
        // Enter); with the kitty keyboard protocol on, `CSI 104;5u` (in hex).
        let argv = helper(&["raw", "print", "ready\\r\\n", "read-line", "said", "print", "\\e[>1ukitty\\r\\n", "hex"]);
        let mut t = Term::spawn(&argv, &Options::default());
        t.wait_text("ready");
        t.key("C-h");
        t.key("Enter");
        t.wait_text("kitty");
        t.key("C-h");
        let kitty = "1b 5b 31 30 34 3b 35 75";
        let s = t.wait_for("the kitty encoding", |s| s.contains(kitty));
        let text = s.text();
        assert!(text.contains("said:\\x08"), "{text}");
        assert!(text.contains(kitty), "{text}");
    }

    #[test]
    #[cfg_attr(not(unix), ignore = "kitty graphics do not pass the in-box ConPTY (doc/terminals/conpty.md)")]
    fn renders_backgrounds_and_images() {
        // A 2x2 RGB image (red, green / blue, white), placed: at (0, 0)
        // offset by (3, 4) px; at row 4 scaled to 2x1 cells; under the
        // backgrounds at row 6 below a red cell and in the default
        // background.
        let img = "f=24,s=2,v=2,C=1,q=2;/wAAAP8AAAD/////";
        let text = format!(
            "\\e_Ga=T,i=1,X=3,Y=4,{img}\\e\\\\\\e[4;1H\\e_Ga=T,i=2,c=2,r=1,{img}\\e\\\\\
             \\e[6;1H\\e[41m  \\e[0m\\e[6;1H\\e_Ga=T,i=3,z=-1073741825,{img}\\e\\\\\
             \\e[6;6H\\e_Ga=T,i=4,z=-1073741825,{img}\\e\\\\\\e[8;1Hdone"
        );
        let mut t = Term::spawn(&helper(&["print", &text, "sleep", "5000"]), &Options::default());
        let s = t.wait_for("the images", |s| s.images.len() == 4 && s.contains("done"));
        let out = s.render();
        assert_eq!((out.width, out.height), (1000, 600));
        let [r, g, b] = s.background;
        let bg = [r, g, b, 255];
        let (red, green, blue) = ([255, 0, 0, 255], [0, 255, 0, 255], [0, 0, 255, 255]);
        // Offsets.
        assert_eq!([out.pixel(2, 4), out.pixel(3, 4), out.pixel(4, 4), out.pixel(3, 5)], [bg, red, green, blue]);
        // Scaled by nearest neighbour to 20x20.
        assert_eq!(
            [out.pixel(9, 69), out.pixel(10, 60), out.pixel(0, 70), out.pixel(19, 79)],
            [red, green, blue, [255; 4]]
        );
        // Under the backgrounds: hidden by the red cells, shown in the default.
        let cell_red = out.pixel(15, 105);
        assert!(cell_red != bg && cell_red[0] > 128, "{cell_red:?}");
        assert_eq!(out.pixel(0, 100), cell_red);
        assert_eq!(out.pixel(50, 100), red);
        // Text is left to the mask.
        let mask = s.text_mask();
        assert_eq!(mask.pixel(5, 145)[3], 255, "\"done\" on row 8");
        assert_eq!(mask.pixel(5, 105)[3], 0);
    }

    #[test]
    #[cfg_attr(not(unix), ignore = "kitty graphics do not pass the in-box ConPTY (doc/terminals/conpty.md)")]
    fn renders_text() {
        // Row 0: red ASCII, then blank cells. Rows 1 and 2: the same text,
        // regular and bold. Row 3: wide characters. Row 4: inverse, in the
        // default colours and in green on blue. Rows 6 and 7: text with a
        // red image over its two cells, below the text (z -1) and above it
        // (z 1).
        let red_image = |id: u32, z: i32| format!("\\e_Ga=T,i={id},c=2,r=1,z={z},C=1,q=2,f=24,s=1,v=1;/wAA\\e\\\\");
        let text = format!(
            "\\e[31mHello\\e[0m\\nHHHH\\n\\e[1mHHHH\\e[0m\\n数😀\\n\
             \\e[7mXX\\e[0m \\e[7;32;44mXX\\e[0m\\n\
             \\e[7;1HWW\\e[7;1H{}\\e[8;1HWW\\e[8;1H{}\\e[10;1Hdone",
            red_image(1, -1),
            red_image(2, 1),
        );
        let mut t = Term::spawn(&helper(&["print", &text, "sleep", "5000"]), &Options::default());
        let s = t.wait_for("the text and images", |s| s.images.len() == 2 && s.contains("done"));
        let out = s.render();
        let _ = std::fs::create_dir_all(out_dir());
        out.save_png(&out_dir().join("renders-text.png")).unwrap();
        let (cw, ch) = s.cell;
        let [r, g, b] = s.background;
        let bg = [r, g, b, 255];
        // The pixels of cell (col, row) (a span of `n` cells) for which `f` holds.
        let count = |col: u32, row: u32, n: u32, f: &dyn Fn([u8; 4]) -> bool| {
            let mut k = 0;
            for y in row * ch..(row + 1) * ch {
                for x in col * cw..(col + n) * cw {
                    k += usize::from(f(out.pixel(x, y)));
                }
            }
            k
        };
        let ink = |col, row, n| count(col, row, n, &|p| p != bg);
        // Red text, reddish pixels in every letter, none in blank cells.
        let red = s.cell(0, 0).fg_rgb;
        assert!(red[0] > red[1].saturating_add(60) && red[0] > red[2].saturating_add(60), "{red:?}");
        for col in 0..5 {
            let reddish = count(col, 0, 1, &|p| p[0] > p[1].saturating_add(40) && p[0] > p[2].saturating_add(40));
            assert!(reddish > 5, "letter {col}: {reddish} red pixels");
        }
        for col in [5, 6, 20, 99] {
            assert_eq!(ink(col, 0, 1), 0, "blank cell {col}");
        }
        assert_eq!(ink(50, 10, 1), 0, "a blank row");
        // Bold has more ink than regular, in the default foreground.
        let fg = s.cell(0, 1).fg_rgb;
        assert_eq!(fg, s.cell(0, 2).fg_rgb);
        assert_ne!(fg, s.background);
        let near = |c: [u8; 3]| move |p: [u8; 4]| (0..3).all(|i| p[i].abs_diff(c[i]) < 60);
        // Ink: how far the pixels are from the background, summed.
        let ink_sum = |row: u32| {
            let mut sum = 0u32;
            for y in row * ch..(row + 1) * ch {
                for x in 0..4 * cw {
                    let p = out.pixel(x, y);
                    sum += (0..3).map(|i| u32::from(p[i].abs_diff(bg[i]))).sum::<u32>();
                }
            }
            sum
        };
        let (regular, bold) = (ink_sum(1), ink_sum(2));
        assert!(regular > 0 && bold > regular * 5 / 4, "regular {regular}, bold {bold}");
        // Wide characters draw in both of their cells.
        assert!(s.cell(1, 3).spacer && s.cell(3, 3).spacer);
        for col in 0..4 {
            assert!(ink(col, 3, 1) > 5, "cell {col} of the wide characters: {}", ink(col, 3, 1));
        }
        assert_eq!(ink(4, 3, 1), 0);
        // Inverse: the cell's background is the foreground, the glyphs the
        // background.
        let inv = s.cell(0, 4);
        assert_eq!((inv.bg_rgb, inv.fg_rgb), (Some(fg), s.background));
        let [fr, fgg, fb] = fg;
        assert_eq!(out.pixel(0, 4 * ch), [fr, fgg, fb, 255]);
        assert!(count(0, 4, 2, &near(s.background)) > 20);
        let green_on_blue = s.cell(3, 4);
        let (green, blue) = (green_on_blue.bg_rgb.unwrap(), green_on_blue.fg_rgb);
        assert!(green[1] > green[2] && blue[2] > blue[1], "inverse swaps: background {green:?}, glyphs {blue:?}");
        assert_eq!(out.pixel(3 * cw, 4 * ch), [green[0], green[1], green[2], 255]);
        assert!(count(3, 4, 2, &near(blue)) > 20);
        // Text over an image with a negative z, under one with z >= 0.
        let image_red = |p: [u8; 4]| p == [255, 0, 0, 255];
        assert!(count(0, 6, 2, &image_red) > 100 && count(0, 6, 2, &near(fg)) > 20, "text above z -1");
        assert_eq!(count(0, 7, 2, &image_red), (2 * cw * ch) as usize, "text below z 1");
    }

    #[test]
    fn exit_codes() {
        let mut t = Term::spawn(&helper(&["exit", "3"]), &Options::default());
        assert_eq!(t.wait_exit(), 3);
    }

    #[test]
    fn inside_tmux() {
        if !have_tmux() {
            eprintln!("SKIPPED: tmux not found");
            return;
        }
        let mut t = Term::inside_tmux(&["sh", "-c", "echo in-tmux; sleep 30"], &Options::default());
        t.wait_text("in-tmux");
        assert!(t.capture_pane().contains("in-tmux"));
        // Nothing is left behind: neither the server's socket nor its
        // configuration file.
        let tmux = t.tmux.as_ref().unwrap();
        let out = tmux.cmd().args(["display-message", "-p", "#{socket_path}"]).output().unwrap();
        let socket = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
        let conf = tmux.conf.clone();
        assert!(socket.exists() && conf.exists());
        drop(t);
        assert!(!socket.exists(), "{socket:?} left");
        assert!(!conf.exists(), "{conf:?} left");
    }
}
