//! `--render-frame`: render one screen of the viewer into a PNG without a
//! terminal. Runs the same `Session` as the interactive mode (with its worker
//! threads), waits until all work is done after each key, and replays the
//! compositor output through termpix's mock terminal, so the image is what a
//! kitty-compatible terminal would show.

use std::path::Path;
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use temzdf_pdfview::host::HostOptions;
use temzdf_scene::Hints;
use temzdf_server::session::{EventSink, Session, SessionEvent, Settings};
use temzdf_tty::Presenter;
use termpix::compositor::{Backend, Compositor};
use termpix::geom::Geometry;
use termpix::graphics::Medium;
use termpix::input::KeyEvent;
use termpix::mock::MockTerminal;

use crate::keys::{Key, Keymap};

pub struct FrameOptions {
    pub geometry: Geometry,
    /// 0-based page to scroll to.
    pub page: Option<usize>,
    /// Zoom (pixels per point); fit width when `None`.
    pub scale: Option<f32>,
    /// Keys to press before rendering, space separated (see [`parse_keys`]).
    pub keys: Option<String>,
    /// Blend translucent layers in linear light (as kitty does).
    pub linear_blending: bool,
}

/// Parse a key list like `"Tab j j Enter C-f 3 G"`, in the notation of
/// [`Key::parse`].
pub fn parse_keys(spec: &str) -> Result<Vec<KeyEvent>> {
    spec.split_whitespace().map(|tok| Ok(Key::parse(tok)?.event())).collect()
}

/// Longest wait for the worker threads.
const WORK_TIMEOUT: Duration = Duration::from_secs(20);

pub fn render_frame(pdf_path: &Path, out: &Path, opts: &FrameOptions) -> Result<()> {
    let name =
        pdf_path.file_name().map_or_else(|| pdf_path.display().to_string(), |n| n.to_string_lossy().into_owned());
    let (tx, rx) = mpsc::channel::<SessionEvent>();
    let tx = Mutex::new(tx);
    let sink: EventSink = Arc::new(move |e| tx.lock().unwrap_or_else(|p| p.into_inner()).send(e).is_ok());
    let settings = Settings {
        keymap: Keymap::default(),
        preview_delay: None,
        anchor_by_text: false,
        scroll: Default::default(),
        host: HostOptions::default(),
        serve_forward: false,
        script: None,
    };
    // A Layered terminal over inline transfers, like the mock terminal.
    let mut session = Session::new(name, opts.geometry, Hints::default(), settings, sink);
    session.open(pdf_path)?;
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
        for _ in 0..200 {
            if !session.is_animating() {
                break;
            }
            now += Duration::from_millis(16);
            session.tick(now);
        }
        // Later keys may depend on the results (search hits, loaded pages).
        settle(&mut session, &rx, now)?;
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
