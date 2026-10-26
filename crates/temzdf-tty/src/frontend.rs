//! A terminal frontend: the terminal itself (set up, probed, restored on drop),
//! the compositor and the pacing of frames. The caller runs the event loop:
//! terminal events come through the channel given to
//! [`TtyFrontend::spawn_input`] and go back in through
//! [`TtyFrontend::handle`]; frames go out through [`TtyFrontend::present`].

use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use temzdf_scene::{Effect, Frame, Hints, ImageUpdate, Images, Pointer};
use termpix::compositor::{Backend, Compositor};
use termpix::geom::Geometry;
use termpix::graphics::Medium;
use termpix::input::{Event, KeyEvent, MouseEvent};
use termpix::probe::Capabilities;
use termpix::tty::{self, Modes, TermEvent, Tty};

use crate::{Presenter, Screen, copy_sequence, pointer_sequence};

/// Raw bytes of not-yet-shown images sent per idle iteration (about four
/// 512 px tiles); small enough that input is never kept waiting.
const UPLOAD_AHEAD_BATCH: usize = 4 << 20;
/// How often to look whether queued output was written, while it is.
const OUTPUT_POLL: Duration = Duration::from_millis(5);
/// Longest wait for the visible pages before showing the first frame.
const FIRST_FRAME_WAIT: Duration = Duration::from_millis(1500);

/// What a terminal event means for the caller.
pub enum Input {
    Key(KeyEvent),
    Mouse(MouseEvent),
    Paste(String),
    /// The terminal changed size.
    Resized(Geometry),
    /// The terminal went away or we were told to stop.
    Terminate,
    /// Handled here; redraw if true.
    Handled(bool),
}

/// Whether [`TtyFrontend::present`] showed the frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Presented {
    Shown,
    /// Its images not now (the frame rate is capped), but its text is shown
    /// when it can be alone; present again soon (or a newer frame).
    Held,
    /// A first frame waiting for its pages to finish rendering (at most
    /// `FIRST_FRAME_WAIT`); a newer frame should replace it.
    Waiting,
}

pub struct TtyFrontend {
    tty: Tty,
    caps: Capabilities,
    comp: Compositor,
    medium: Medium,
    presenter: Presenter,
    screen: Screen,
    pointer: Pointer,
    started: Instant,
    first_shown: bool,
    /// When the last frame that sent pixels was shown.
    last_pixels: Option<Instant>,
    /// Composited frames resend whole strips of the screen; multiplexers
    /// (zellij 0.45 forwards them uncompressed) and inline links pay for every
    /// one, so their rate is capped. Layered frames are tiny placement updates.
    min_interval: Duration,
    /// The last present was held or found the terminal busy.
    retry_soon: bool,
    uploading: bool,
}

impl TtyFrontend {
    /// Open and probe the terminal. `choose` picks the backend and transfer
    /// medium from what detection found (configuration overrides) and the zlib
    /// level for inline transfers.
    pub fn open(choose: impl FnOnce(&Capabilities) -> (Backend, Medium, u32)) -> Result<Self> {
        let mut tty = Tty::open(Modes::default())?;
        let caps = termpix::probe::probe(&mut tty, Duration::from_secs(1))?;
        if !caps.kitty_graphics {
            drop(tty);
            match &caps.tmux {
                Some(t) if !t.passthrough => {
                    bail!("inside tmux, graphics need passthrough: run `tmux set -g allow-passthrough on`")
                }
                Some(t) => bail!(
                    "inside tmux, graphics need an outer terminal with Unicode placeholder support \
                     (kitty, Ghostty); tmux reports {:?}",
                    t.outer
                ),
                None if caps.in_tmux => bail!("inside tmux, but the `tmux` command could not be queried"),
                None => bail!("this terminal does not support the kitty graphics protocol"),
            }
        }
        if caps.pixel_mouse {
            tty.enable_pixel_mouse()?;
        }
        let (backend, medium, zlib_level) = choose(&caps);
        let mut comp = Compositor::new(backend, medium, caps.geometry);
        // In the log (stderr is redirected): scripts/e2e.sh reads the geometry to
        // render the expected screen headlessly.
        eprintln!("temzdf: {} with {backend:?} over {medium:?}", geometry_line(caps.geometry));
        comp.encoder_mut().set_zlib_level(zlib_level);
        if caps.in_tmux {
            comp.set_tmux_passthrough(true);
            tty.set_tmux_passthrough();
        }
        let composited = matches!(backend, Backend::Composited { .. } | Backend::Placeholders { .. });
        // tmux passes our (compressed) bytes through as they are; zellij
        // re-encodes every image uncompressed. Inline transfers are slow over
        // SSH, but local terminals without shm or temp files (WezTerm on
        // Linux) take them fast.
        let slow_link = caps.in_zellij || (!caps.in_tmux && matches!(medium, Medium::Direct { .. }) && over_ssh());
        let min_interval = match (composited, slow_link) {
            (false, _) => Duration::ZERO,
            (true, false) => Duration::from_millis(33),
            (true, true) => Duration::from_millis(200),
        };
        Ok(Self {
            tty,
            caps,
            comp,
            medium,
            presenter: Presenter::new(),
            screen: Screen::new(),
            pointer: Pointer::Default,
            started: Instant::now(),
            first_shown: false,
            last_pixels: None,
            min_interval,
            retry_soon: false,
            uploading: false,
        })
    }

    /// Read terminal events on a thread, wrapped into the caller's event type.
    pub fn spawn_input<T: Send + 'static>(
        &mut self,
        tx: Sender<T>,
        wrap: impl Fn(TermEvent) -> T + Send + Sync + Clone + 'static,
    ) -> Result<()> {
        tty::spawn_input(self.tty.reader()?, self.caps.pixel_mouse, tx, wrap)?;
        Ok(())
    }

    pub fn geometry(&self) -> Geometry {
        self.comp.geometry()
    }

    pub fn hints(&self) -> Hints {
        Hints {
            exact_scaling: !matches!(self.comp.backend(), Backend::Layered),
            fast_uploads: !matches!(self.medium, Medium::Direct { .. }),
        }
    }

    /// Mirror a pane's image store (see [`Presenter::sync`]).
    pub fn sync(&mut self, images: &mut Images) {
        self.presenter.sync(&mut self.comp, images);
    }

    /// Apply image changes received from elsewhere (see [`Presenter::apply`]).
    pub fn apply(&mut self, updates: Vec<ImageUpdate>) {
        self.presenter.apply(&mut self.comp, updates);
    }

    /// Whether the terminal can take a frame now. On a slow link, wait until it
    /// caught up with earlier frames; the frame drawn afterwards shows the
    /// latest state anyway. Output is written by a background thread (input
    /// stays responsive meanwhile); frames queued behind it would only pile up.
    pub fn ready(&mut self) -> bool {
        let ready = self.tty.pending_output() == 0 && !self.comp.is_congested(Instant::now());
        self.retry_soon |= !ready;
        ready
    }

    /// Show a frame, unless it should wait: half-rendered first frames are not
    /// shown (each costs a full upload in Composited mode) until
    /// `FIRST_FRAME_WAIT`, and the frame-rate cap applies to frames that send
    /// pixels. `complete` says
    /// whether everything visible is fully rendered.
    pub fn present(&mut self, frame: &Frame, complete: bool) -> Result<Presented> {
        if !self.first_shown && !complete && self.started.elapsed() < FIRST_FRAME_WAIT {
            self.retry_soon = true;
            return Ok(Presented::Waiting);
        }
        // The cap is for frames that send pixels; text alone (a shell echoing
        // keys) goes out at once.
        let pixels = self.comp.would_resend(&self.presenter.layers(&frame.layers));
        if pixels && self.last_pixels.is_some_and(|t| t.elapsed() < self.min_interval) {
            self.retry_soon = true;
            // A PDF scrolling beside a shell: the shell's echo goes out now.
            if let Some(bytes) = self.screen.render_text(frame, &mut self.comp)
                && !bytes.is_empty()
            {
                self.tty.write_all(bytes)?;
            }
            self.set_pointer(frame.pointer)?;
            return Ok(Presented::Held);
        }
        let bytes = self.screen.render(frame, &self.presenter, &mut self.comp)?;
        if !bytes.is_empty() {
            self.tty.write_all(bytes)?;
        }
        self.set_pointer(frame.pointer)?;
        self.first_shown = true;
        if pixels {
            self.last_pixels = Some(Instant::now());
        }
        Ok(Presented::Shown)
    }

    /// Update the mouse pointer shape (OSC 22) if it changed.
    pub fn set_pointer(&mut self, pointer: Pointer) -> Result<()> {
        if pointer != self.pointer {
            self.pointer = pointer;
            self.tty.write_all(&pointer_sequence(pointer))?;
        }
        Ok(())
    }

    /// While nothing needs drawing and the link is free: send pre-rendered
    /// images now, so showing them later costs only a placement (matters over SSH).
    pub fn idle(&mut self) -> Result<()> {
        self.uploading = false;
        if self.tty.pending_output() == 0 && !self.comp.is_congested(Instant::now()) && self.comp.has_pending_uploads()
        {
            let mut ahead = Vec::new();
            self.uploading = self.comp.upload_ahead(&mut ahead, UPLOAD_AHEAD_BATCH)?;
            if !ahead.is_empty() {
                self.tty.write_all(&ahead)?;
            }
        }
        Ok(())
    }

    /// The longest the caller may wait for events before calling again.
    pub fn timeout(&mut self, mut timeout: Duration) -> Duration {
        if std::mem::take(&mut self.retry_soon) || self.uploading {
            // Re-check soon: the acknowledgement may never come, the frame-rate
            // interval passes, or the first frame stops waiting.
            timeout = timeout.min(Duration::from_millis(30));
        }
        if self.tty.pending_output() > 0 {
            // Nothing signals the end of a write; poll for it.
            timeout = timeout.min(OUTPUT_POLL);
        }
        timeout
    }

    /// The terminal's size after a resize: the in-band report (mode 2048,
    /// pixels included) if that is what came, else the kernel's, keeping the
    /// cell size when it reports no pixels.
    fn new_geometry(&self, ev: &TermEvent) -> Result<Option<Geometry>> {
        if let TermEvent::Input(Event::Resize(r)) = ev
            && let Some(g) = r.geometry()
        {
            return Ok(Some(g));
        }
        Ok(self.tty.winsize()?.geometry_or(self.comp.geometry().cell))
    }

    /// Interpret a terminal event.
    pub fn handle(&mut self, ev: TermEvent) -> Result<Input> {
        Ok(match ev {
            TermEvent::Terminate => Input::Terminate,
            TermEvent::WindowChanged | TermEvent::Input(Event::Resize(_)) => match self.new_geometry(&ev)? {
                Some(g) if g != self.comp.geometry() => {
                    self.comp.set_geometry(g);
                    eprintln!("temzdf: resized to {}", geometry_line(g));
                    Input::Resized(g)
                }
                _ => Input::Handled(false),
            },
            TermEvent::Input(Event::Graphics(reply)) => Input::Handled(self.comp.handle_reply(&reply)),
            TermEvent::Input(Event::Key(key)) => Input::Key(key),
            TermEvent::Input(Event::Mouse(m)) => Input::Mouse(m),
            TermEvent::Input(Event::Paste(text)) => Input::Paste(text),
            _ => Input::Handled(false),
        })
    }

    /// Carry out an effect a pane asked for, in this frontend's environment.
    /// Failures to open a link are reported through `on_failure` (possibly
    /// later, from another thread).
    pub fn perform(&mut self, effect: Effect, on_failure: impl Fn(String) + Send + 'static) -> Result<()> {
        match effect {
            Effect::Copy(text) => self.tty.write_all(&copy_sequence(&text, self.caps.in_tmux))?,
            Effect::OpenUri { uri, command } => {
                let default = vec![if cfg!(target_os = "macos") { "open" } else { "xdg-open" }.to_owned()];
                let command = command.filter(|c| !c.is_empty()).unwrap_or(default);
                open_uri(&command, &uri, on_failure)
            }
            Effect::RunShell(cmd) => run_shell(&cmd),
        }
        Ok(())
    }
}

/// "geometry COLSxROWS cells of WxH px" (the format scripts/e2e.sh parses).
/// Whether this process runs in an SSH session (sshd sets these for the
/// login shell; children inherit them, so do tmux panes started there).
fn over_ssh() -> bool {
    ["SSH_CONNECTION", "SSH_CLIENT", "SSH_TTY"].iter().any(|v| std::env::var_os(v).is_some())
}

fn geometry_line(g: Geometry) -> String {
    format!("geometry {}x{} cells of {}x{} px", g.cols, g.rows, g.cell.width, g.cell.height)
}

/// Run a shell command detached from the terminal (inverse search).
fn run_shell(cmd: &str) {
    let _ = std::process::Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

/// Open a URI with the configured handler, detached from the terminal. A
/// handler that cannot start or exits with an error (e.g. `xdg-open` over SSH
/// without a display) is reported through `on_failure`.
fn open_uri(command: &[String], uri: &str, on_failure: impl Fn(String) + Send + 'static) {
    let Some((program, args)) = command.split_first() else { return };
    let child = std::process::Command::new(program)
        .args(args)
        .arg(uri)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    let uri = uri.to_owned();
    let fail = move |why: String| on_failure(format!("cannot open {uri}: {why}"));
    match child {
        Err(e) => fail(format!("{program}: {e}")),
        Ok(mut child) => {
            let program = program.clone();
            std::thread::spawn(move || match child.wait() {
                Ok(status) if !status.success() => fail(format!("{program} failed ({status})")),
                _ => {}
            });
        }
    }
}
