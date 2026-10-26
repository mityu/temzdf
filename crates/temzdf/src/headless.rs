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
/// 200   move 300 200            move: X Y [MODS]
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
