//! `--render-frame`: render one screen of the viewer into a PNG without a
//! terminal. Runs the same `Session` as the interactive mode (with its worker
//! threads), waits until all work is done after each key, and replays the
//! compositor output through termpix's mock terminal, so the image is what a
//! kitty-compatible terminal would show.
//!
//! Input comes as keys (`--keys`, each run to its end) or as a timeline of
//! keys and mouse events (`--events`, see [`parse_events`]) replayed on a
//! simulated clock: wheel acceleration, easing, link previews and other
//! timers see the same times as in the run that was recorded.

use std::path::Path;
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use temzdf_scene::Hints;
use temzdf_server::session::{EventSink, Session, SessionEvent};
use temzdf_tty::Presenter;
use termpix::compositor::{Backend, Compositor};
use termpix::geom::Geometry;
use termpix::graphics::Medium;
use termpix::input::{CoordUnit, KeyEvent, Modifiers, MouseButton, MouseEvent, MouseEventKind, ScrollDirection};
use termpix::mock::MockTerminal;

pub struct FrameOptions {
    pub geometry: Geometry,
    /// 0-based page to scroll to.
    pub page: Option<usize>,
    /// Zoom (pixels per point); fit width when `None`.
    pub scale: Option<f32>,
    /// Keys to press before rendering (see [`parse_keys`]).
    pub keys: Option<String>,
    /// Or a timeline of input to replay (see [`parse_events`]).
    pub events: Option<Vec<Timed>>,
    /// Blend translucent layers in linear light (as kitty does).
    pub linear_blending: bool,
}

/// Parse keys in Vim's `feedkeys` notation (`jjj++02G/needle<CR>`).
pub fn parse_keys(spec: &str) -> Result<Vec<KeyEvent>> {
    if spec.is_empty() {
        return Ok(Vec::new());
    }
    Ok(temzdf_server::keymap::notation::parse(spec)?.into_iter().map(|k| k.event()).collect())
}

/// An input of a replay, at a time since the start.
#[derive(Debug)]
pub struct Timed {
    pub at: Duration,
    pub input: Input,
}

#[derive(Debug)]
pub enum Input {
    Key(KeyEvent),
    Mouse(MouseEvent),
    /// Render the screen as it is at this time.
    End,
}

/// Parse a timeline, one input per line, times in milliseconds since the
/// start, coordinates in pixels of the screen (`#` starts a comment):
///
/// ```text
/// 0     key jj<C-f>             keys in Vim's feedkeys notation
/// 120   press 160 412 left      press, release, drag: X Y BUTTON [MODS]
/// 150   drag 160 430 left
/// 180   release 160 430 left
/// 200   move 300 200            move, leave: X Y [MODS]
/// 230   wheel down 300 200 C    wheel up|down|left|right X Y [MODS]
/// 1500  end                     render at this time (else when settled)
/// ```
///
/// MODS are letters: C (Ctrl), S (Shift), A (Alt).
pub fn parse_events(text: &str) -> Result<Vec<Timed>> {
    let mut out = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.split_once('#').map_or(line, |(l, _)| l).trim();
        if line.is_empty() {
            continue;
        }
        let bad = || anyhow::anyhow!("line {}: {line:?}", n + 1);
        let mut words = line.split_whitespace();
        let at = Duration::from_millis(words.next().and_then(|w| w.parse().ok()).ok_or_else(bad)?);
        let what = words.next().ok_or_else(bad)?;
        if what == "key" {
            let keys = line.splitn(3, char::is_whitespace).nth(2).unwrap_or("").trim();
            out.extend(parse_keys(keys)?.into_iter().map(|k| Timed { at, input: Input::Key(k) }));
            continue;
        }
        if what == "end" {
            out.push(Timed { at, input: Input::End });
            continue;
        }
        let words: Vec<&str> = words.collect();
        let (dir, rest) = match what {
            "wheel" => (words.first().copied(), words.get(1..).unwrap_or(&[])),
            _ => (None, &words[..]),
        };
        let num = |i: usize| rest.get(i).and_then(|w| w.parse::<i32>().ok()).ok_or_else(bad);
        let (x, y) = (num(0)?, num(1)?);
        let button = || match rest.get(2).copied() {
            Some("left") => Ok(MouseButton::Left),
            Some("middle") => Ok(MouseButton::Middle),
            Some("right") => Ok(MouseButton::Right),
            _ => Err(bad()),
        };
        let (kind, mods) = match what {
            "press" => (MouseEventKind::Press(button()?), rest.get(3)),
            "release" => (MouseEventKind::Release(button()?), rest.get(3)),
            "drag" => (MouseEventKind::Drag(button()?), rest.get(3)),
            "move" => (MouseEventKind::Move, rest.get(2)),
            "leave" => (MouseEventKind::Leave, rest.get(2)),
            "wheel" => {
                let dir = match dir {
                    Some("up") => ScrollDirection::Up,
                    Some("down") => ScrollDirection::Down,
                    Some("left") => ScrollDirection::Left,
                    Some("right") => ScrollDirection::Right,
                    _ => return Err(bad()),
                };
                (MouseEventKind::Scroll(dir), rest.get(2))
            }
            _ => return Err(bad()),
        };
        let mut modifiers = Modifiers::NONE;
        for c in mods.copied().unwrap_or("").chars() {
            modifiers |= match c {
                'C' => Modifiers::CTRL,
                'S' => Modifiers::SHIFT,
                'A' => Modifiers::ALT,
                _ => return Err(bad()),
            };
        }
        out.push(Timed { at, input: Input::Mouse(MouseEvent { kind, x, y, unit: CoordUnit::Pixel, modifiers }) });
    }
    Ok(out)
}

/// Writes the input an interactive viewer handles, as a timeline for
/// [`parse_events`] (`TEMZDF_RECORD_INPUT=FILE`): with the viewer's own clock
/// and the terminal's own coordinates, so a replay sees what the viewer saw
/// (not what a test sent: terminals deliver late, batch, or offset).
pub struct Recorder {
    out: std::io::BufWriter<std::fs::File>,
    start: Instant,
}

impl Recorder {
    /// From `TEMZDF_RECORD_INPUT`, if set.
    pub fn from_env() -> Result<Option<Self>> {
        let Some(path) = std::env::var_os("TEMZDF_RECORD_INPUT") else { return Ok(None) };
        let file = std::fs::File::create(&path).with_context(|| format!("TEMZDF_RECORD_INPUT={path:?}"))?;
        Ok(Some(Self { out: std::io::BufWriter::new(file), start: Instant::now() }))
    }

    fn line(&mut self, now: Instant, what: &str) {
        use std::io::Write;
        let ms = now.saturating_duration_since(self.start).as_millis();
        let _ = writeln!(self.out, "{ms} {what}");
        let _ = self.out.flush();
    }

    pub fn key(&mut self, k: &KeyEvent, now: Instant) {
        if k.kind == termpix::input::KeyEventKind::Release || matches!(k.code, termpix::input::KeyCode::Modifier(_)) {
            return;
        }
        let keys = temzdf_server::keymap::Stroke::of(k).notation();
        self.line(now, &format!("key {keys}"));
    }

    /// Mouse events in pixels (cells, as inside multiplexers, are not
    /// recorded: a replay needs the pixel).
    pub fn mouse(&mut self, m: &MouseEvent, now: Instant) {
        if m.unit != CoordUnit::Pixel {
            return;
        }
        let button = |b: &MouseButton| match b {
            MouseButton::Left => Some("left"),
            MouseButton::Middle => Some("middle"),
            MouseButton::Right => Some("right"),
            _ => None,
        };
        let what = match &m.kind {
            MouseEventKind::Press(b) => button(b).map(|b| format!("press {} {} {b}", m.x, m.y)),
            MouseEventKind::Release(b) => button(b).map(|b| format!("release {} {} {b}", m.x, m.y)),
            MouseEventKind::Drag(b) => button(b).map(|b| format!("drag {} {} {b}", m.x, m.y)),
            MouseEventKind::Move => Some(format!("move {} {}", m.x, m.y)),
            MouseEventKind::Leave => Some(format!("leave {} {}", m.x, m.y)),
            MouseEventKind::Scroll(d) => {
                let d = match d {
                    ScrollDirection::Up => "up",
                    ScrollDirection::Down => "down",
                    ScrollDirection::Left => "left",
                    ScrollDirection::Right => "right",
                };
                Some(format!("wheel {d} {} {}", m.x, m.y))
            }
        };
        let Some(mut what) = what else { return };
        let mods: String = [(Modifiers::CTRL, 'C'), (Modifiers::SHIFT, 'S'), (Modifiers::ALT, 'A')]
            .into_iter()
            .filter(|(f, _)| m.modifiers.contains(*f))
            .map(|(_, c)| c)
            .collect();
        if !mods.is_empty() {
            what = format!("{what} {mods}");
        }
        self.line(now, &what);
    }
}

/// The simulated clock's step while time passes in a replay (a frame).
const STEP: Duration = Duration::from_millis(16);

/// Let simulated time pass until `to`: timers, animations, and whatever the
/// workers finished meanwhile.
fn advance(session: &mut Session, rx: &Receiver<SessionEvent>, now: &mut Instant, to: Instant) {
    while *now < to {
        *now = (*now + STEP).min(to);
        session.poll(*now);
        while let Ok(ev) = rx.try_recv() {
            session.handle(ev, *now);
        }
    }
}

/// Run animations started by an input to their end (at most 3.2 s).
fn run_out(session: &mut Session, now: &mut Instant) {
    for _ in 0..200 {
        if !session.is_animating() {
            break;
        }
        *now += STEP;
        session.tick(*now);
    }
}

/// Longest wait for the worker threads.
const WORK_TIMEOUT: Duration = Duration::from_secs(20);

pub fn render_frame(pdf_path: &Path, out: &Path, opts: &FrameOptions) -> Result<()> {
    let name =
        pdf_path.file_name().map_or_else(|| pdf_path.display().to_string(), |n| n.to_string_lossy().into_owned());
    let (tx, rx) = mpsc::channel::<SessionEvent>();
    let tx = Mutex::new(tx);
    let sink: EventSink = Arc::new(move |e| tx.lock().unwrap_or_else(|p| p.into_inner()).send(e).is_ok());
    // The shipped default configuration maps the keys (no user configuration).
    let script = crate::app::script_settings(true, None)?;
    let settings = crate::app::settings(false, script, None);
    // A Layered terminal over inline transfers, like the mock terminal.
    let mut session = Session::new(name, opts.geometry, Hints::default(), settings, sink);
    session.start(temzdf_server::action::PaneSpec::Pdf(pdf_path.to_owned()), None)?;
    // Simulated clock, so animations started by keys settle deterministically.
    let mut now = Instant::now();
    while session.is_starting() {
        let ev = rx.recv_timeout(WORK_TIMEOUT).context("opening the document")?;
        session.handle(ev, now);
    }
    if let Some(e) = session.error() {
        bail!("{e}");
    }
    if let Some(pane) = session.focused_pane() {
        if let Some(scale) = opts.scale {
            pane.viewer.set_scale(scale, (0.0, 0.0));
        }
        if let Some(page) = opts.page {
            pane.viewer.goto_page(page, false, now);
        }
    }
    let mut comp = Compositor::new(Backend::Layered, Medium::Direct { compress: false }, opts.geometry);
    let mut presenter = Presenter::new();
    for key in parse_keys(opts.keys.as_deref().unwrap_or(""))? {
        session.key(&key, now);
        run_out(&mut session, &mut now);
        // Later keys may depend on the results (search hits, loaded pages).
        settle(&mut session, &rx, now)?;
    }
    if let Some(events) = &opts.events {
        // The first screen is up (its pages and links loaded) before any
        // recorded input, as in a terminal.
        settle(&mut session, &rx, now)?;
        let start = now;
        let mut ended = false;
        for t in events {
            advance(&mut session, &rx, &mut now, start + t.at);
            match &t.input {
                Input::Key(k) => {
                    session.key(k, now);
                }
                Input::Mouse(m) => {
                    session.mouse(m, now);
                }
                Input::End => {
                    ended = true;
                    break;
                }
            }
            settle(&mut session, &rx, now)?;
        }
        if !ended {
            run_out(&mut session, &mut now);
        }
    }
    let frame = settle(&mut session, &rx, now)?;
    presenter.sync(&mut comp, session.images());
    let mut bytes = Vec::new();
    comp.present(&presenter.layers(&frame.layers), &mut bytes)?;
    let mut term = MockTerminal::new(opts.geometry, [0, 0, 0, 255]);
    if opts.linear_blending {
        term.set_blending(termpix::raster::Blending::Linear);
    }
    term.feed(&bytes);
    term.render().save_png(out)?;
    Ok(())
}

/// Build frames and handle the workers' results until nothing is left to do;
/// returns the final frame.
fn settle(session: &mut Session, rx: &Receiver<SessionEvent>, now: Instant) -> Result<temzdf_scene::Frame> {
    loop {
        // Requests (searches) go to the workers; effects have nowhere to go.
        session.take_effects();
        let frame = session.frame();
        if session.is_idle() {
            return Ok(frame);
        }
        let ev = rx.recv_timeout(WORK_TIMEOUT).context("waiting for the document's threads")?;
        session.handle(ev, now);
        while let Ok(ev) = rx.try_recv() {
            session.handle(ev, now);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_the_recorder_writes_replays_as_the_same_input() {
        let path = std::env::temp_dir().join(format!("temzdf-record-{}.txt", std::process::id()));
        let file = std::fs::File::create(&path).unwrap();
        let start = Instant::now();
        let mut r = Recorder { out: std::io::BufWriter::new(file), start };
        let at = |ms| start + Duration::from_millis(ms);
        let mouse = |kind, modifiers| MouseEvent { kind, x: 451, y: 301, unit: CoordUnit::Pixel, modifiers };
        let input = [
            (0, Input::Key(parse_keys("<C-f>").unwrap()[0].clone())),
            (5, Input::Key(parse_keys("<lt>").unwrap()[0].clone())),
            (30, Input::Mouse(mouse(MouseEventKind::Scroll(ScrollDirection::Down), Modifiers::CTRL))),
            (60, Input::Mouse(mouse(MouseEventKind::Press(MouseButton::Left), Modifiers::NONE))),
            (90, Input::Mouse(mouse(MouseEventKind::Drag(MouseButton::Left), Modifiers::SHIFT))),
            (95, Input::Mouse(mouse(MouseEventKind::Release(MouseButton::Left), Modifiers::NONE))),
            (99, Input::Mouse(mouse(MouseEventKind::Leave, Modifiers::NONE))),
        ];
        for (ms, i) in &input {
            match i {
                Input::Key(k) => r.key(k, at(*ms)),
                Input::Mouse(m) => r.mouse(m, at(*ms)),
                Input::End => {}
            }
        }
        // Cells (inside multiplexers) are left out.
        r.mouse(&MouseEvent { unit: CoordUnit::Cell, ..mouse(MouseEventKind::Move, Modifiers::NONE) }, at(120));
        drop(r);
        let back = parse_events(&std::fs::read_to_string(&path).unwrap()).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(back.len(), input.len());
        for ((ms, want), got) in input.iter().zip(&back) {
            assert_eq!(got.at, Duration::from_millis(*ms));
            match (want, &got.input) {
                (Input::Key(a), Input::Key(b)) => assert_eq!((a.code, a.modifiers), (b.code, b.modifiers)),
                (Input::Mouse(a), Input::Mouse(b)) => assert_eq!(a, b),
                other => panic!("{other:?}"),
            }
        }
    }
}
