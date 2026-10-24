//! Startup capability probing: which terminal this is, whether it speaks kitty
//! graphics, which transmission media work, which modes it supports, and the
//! cell size. Decisions are summarised in [`Capabilities::backend`] and
//! [`Capabilities::medium`]; see `doc/terminal-compat.md` for the reasoning.
//!
//! Environment overrides (for when detection is wrong):
//! - `TERMPIX_BACKEND=layered|composited`
//! - `TERMPIX_MEDIUM=direct|shm|file`

use std::io;
use std::time::{Duration, Instant};

use crate::compositor::Backend;
use crate::geom::{CellSize, Geometry};
use crate::graphics::{self, Medium};
use crate::input::{Event, Parser, Reply};
use crate::tty::Tty;

const ID_DIRECT: u32 = 0x7e_0001;
const ID_SHM: u32 = 0x7e_0002;
const ID_FILE: u32 = 0x7e_0003;
const FALLBACK_CELL: CellSize = CellSize { width: 8, height: 16 };

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalKind {
    Kitty,
    Ghostty,
    WezTerm,
    Konsole,
    ITerm2,
    Foot,
    /// Multiplexers answer XTVERSION themselves; the outer terminal is hidden.
    Zellij,
    Tmux,
    Other(String),
    Unknown,
}

impl TerminalKind {
    fn from_version(v: &str) -> Self {
        let l = v.to_ascii_lowercase();
        if l.starts_with("kitty") {
            Self::Kitty
        } else if l.starts_with("ghostty") {
            Self::Ghostty
        } else if l.starts_with("wezterm") {
            Self::WezTerm
        } else if l.starts_with("konsole") {
            Self::Konsole
        } else if l.starts_with("iterm2") {
            Self::ITerm2
        } else if l.starts_with("foot") {
            Self::Foot
        } else if l.starts_with("zellij") {
            Self::Zellij
        } else if l.starts_with("tmux") {
            Self::Tmux
        } else {
            Self::Other(v.to_owned())
        }
    }

    fn from_env(env: &Env) -> Self {
        if env.kitty_window {
            return Self::Kitty;
        }
        match env.term_program.as_deref() {
            Some("ghostty") => Self::Ghostty,
            Some("WezTerm") => Self::WezTerm,
            Some("iTerm.app") => Self::ITerm2,
            _ if env.konsole => Self::Konsole,
            _ => Self::Unknown,
        }
    }
}

/// Environment hints, captured once so detection logic is testable.
#[derive(Clone, Debug, Default)]
pub struct Env {
    pub term_program: Option<String>,
    pub kitty_window: bool,
    pub konsole: bool,
    pub tmux: bool,
    pub zellij: bool,
    pub backend_override: Option<String>,
    pub medium_override: Option<String>,
}

impl Env {
    pub fn from_process() -> Self {
        let var = |k| std::env::var(k).ok().filter(|v: &String| !v.is_empty());
        Self {
            term_program: var("TERM_PROGRAM"),
            kitty_window: var("KITTY_WINDOW_ID").is_some(),
            konsole: var("KONSOLE_VERSION").is_some(),
            tmux: var("TMUX").is_some(),
            zellij: var("ZELLIJ").is_some(),
            backend_override: var("TERMPIX_BACKEND"),
            medium_override: var("TERMPIX_MEDIUM"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Capabilities {
    pub terminal: TerminalKind,
    pub version: Option<String>,
    pub kitty_graphics: bool,
    pub shm: bool,
    pub temp_file: bool,
    /// SGR-pixel mouse (1016).
    pub pixel_mouse: bool,
    /// Synchronized output (2026).
    pub sync_output: bool,
    /// Kitty keyboard protocol flags currently active, if the protocol is supported.
    pub keyboard_flags: Option<u32>,
    pub geometry: Geometry,
    pub in_tmux: bool,
    pub in_zellij: bool,
    /// What tmux reports about itself and the outer terminal, when inside tmux.
    pub tmux: Option<Tmux>,
    backend_override: Option<Backend>,
    medium_override: Option<Medium>,
}

/// tmux settings that decide whether graphics can work through it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tmux {
    /// `allow-passthrough` is `on` or `all` (pane value, else global).
    pub passthrough: bool,
    /// The outer terminal as tmux sees it (`client_termtype`, else
    /// `client_termname`).
    pub outer: String,
}

impl Tmux {
    /// Graphics through tmux need passthrough and an outer terminal with
    /// Unicode placeholder support (kitty, Ghostty; not WezTerm).
    pub fn outer_supports_placeholders(&self) -> bool {
        let o = self.outer.to_ascii_lowercase();
        o.contains("kitty") || o.contains("ghostty")
    }

    /// From the outputs of `tmux show-options -pv/-gv allow-passthrough` and
    /// `tmux display -p '#{client_termtype}'` / `'#{client_termname}'`.
    pub fn from_outputs(pane: &str, global: &str, termtype: &str, termname: &str) -> Self {
        let value = if pane.trim().is_empty() { global.trim() } else { pane.trim() };
        let outer = if termtype.trim().is_empty() { termname.trim() } else { termtype.trim() };
        Self { passthrough: matches!(value, "on" | "all"), outer: outer.to_owned() }
    }

    /// Ask the tmux server (via the `tmux` command) about this client and pane.
    pub fn query() -> Option<Self> {
        let run = |args: &[&str]| {
            std::process::Command::new("tmux")
                .args(args)
                .stderr(std::process::Stdio::null())
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        };
        let global = run(&["show-options", "-gqv", "allow-passthrough"])?;
        let pane = run(&["show-options", "-pqv", "allow-passthrough"]).unwrap_or_default();
        let termtype = run(&["display-message", "-p", "#{client_termtype}"]).unwrap_or_default();
        let termname = run(&["display-message", "-p", "#{client_termname}"]).unwrap_or_default();
        Some(Self::from_outputs(&pane, &global, &termtype, &termname))
    }
}

impl Capabilities {
    /// Whether `TERMPIX_BACKEND` forced the backend.
    pub fn has_backend_override(&self) -> bool {
        self.backend_override.is_some()
    }

    /// Whether `TERMPIX_MEDIUM` forced the medium.
    pub fn has_medium_override(&self) -> bool {
        self.medium_override.is_some()
    }

    /// Compositing backend to use.
    pub fn backend(&self) -> Backend {
        if let Some(b) = self.backend_override {
            return b;
        }
        // tmux shows images only through Unicode placeholders, which it keeps
        // and redraws as ordinary text.
        if self.in_tmux {
            return Backend::placeholders(2);
        }
        // WezTerm and zellij mishandle placements (moves, z-order, crops).
        if self.terminal == TerminalKind::WezTerm || self.in_zellij {
            // zellij forwards every image uncompressed: strips keep updates
            // small. WezTerm garbles side-by-side images: one whole frame.
            if self.in_zellij { Backend::composited_strips(2) } else { Backend::composited() }
        } else {
            Backend::Layered
        }
    }

    /// Transmission medium to use.
    pub fn medium(&self) -> Medium {
        if let Some(m) = self.medium_override {
            return m;
        }
        if self.shm {
            Medium::SharedMemory
        } else if self.temp_file {
            Medium::TempFile
        } else {
            Medium::Direct { compress: true }
        }
    }
}

/// Query the terminal. Must run before the input thread starts (it reads the tty).
pub fn probe(tty: &mut Tty, timeout: Duration) -> io::Result<Capabilities> {
    let env = Env::from_process();
    let mut q = Vec::new();
    q.extend_from_slice(b"\x1b[>0q"); // XTVERSION
    graphics::query(&mut q, ID_DIRECT, Medium::Direct { compress: false })?;
    // shm and temp-file probes are only meaningful on the same host; failures are fine.
    let _ = graphics::query(&mut q, ID_SHM, Medium::SharedMemory);
    let _ = graphics::query(&mut q, ID_FILE, Medium::TempFile);
    q.extend_from_slice(b"\x1b[?1016$p\x1b[?2026$p\x1b[?u\x1b[16t\x1b[c");
    tty.write_all(&q)?;

    let mut parser = Parser::new();
    let mut replies = Vec::new();
    let deadline = Instant::now() + timeout;
    let mut buf = [0u8; 1024];
    'outer: while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        let Some(n) = tty.read_timeout(&mut buf, Some(left))? else { break };
        if n == 0 {
            break;
        }
        for ev in parser.feed(&buf[..n]) {
            let done = matches!(ev, Event::Reply(Reply::DeviceAttributes(_)));
            replies.push(ev);
            if done {
                break 'outer;
            }
        }
    }
    let winsize = tty.winsize()?;
    let mut caps = interpret(&env, &replies, winsize.geometry(), (winsize.cols, winsize.rows));
    if caps.in_tmux {
        // tmux answers graphics queries itself (or not at all), so ask tmux
        // what is on the other side instead.
        caps.tmux = Tmux::query();
        caps.kitty_graphics = caps.tmux.as_ref().is_some_and(|t| t.passthrough && t.outer_supports_placeholders());
    }
    Ok(caps)
}

/// Turn probe replies into capabilities. Pure, for tests.
pub fn interpret(env: &Env, replies: &[Event], kernel: Option<Geometry>, grid: (u16, u16)) -> Capabilities {
    let mut caps = Capabilities {
        terminal: TerminalKind::from_env(env),
        version: None,
        kitty_graphics: false,
        shm: false,
        temp_file: false,
        pixel_mouse: false,
        sync_output: false,
        keyboard_flags: None,
        geometry: Geometry { cols: grid.0.max(1), rows: grid.1.max(1), cell: FALLBACK_CELL },
        in_tmux: env.tmux,
        in_zellij: env.zellij,
        tmux: None,
        backend_override: match env.backend_override.as_deref() {
            Some("layered") => Some(Backend::Layered),
            Some("composited") => Some(Backend::composited()),
            _ => None,
        },
        medium_override: match env.medium_override.as_deref() {
            Some("direct") => Some(Medium::Direct { compress: true }),
            Some("shm") => Some(Medium::SharedMemory),
            Some("file") => Some(Medium::TempFile),
            _ => None,
        },
    };
    let mut reported_cell = None;
    for ev in replies {
        match ev {
            Event::Graphics(r) => {
                let ok = r.is_ok();
                match r.image_id {
                    Some(ID_DIRECT) => caps.kitty_graphics |= ok,
                    Some(ID_SHM) => {
                        caps.kitty_graphics = true;
                        caps.shm = ok;
                    }
                    Some(ID_FILE) => {
                        caps.kitty_graphics = true;
                        caps.temp_file = ok;
                    }
                    _ => {}
                }
            }
            Event::Reply(Reply::Version(v)) => {
                caps.terminal = TerminalKind::from_version(v);
                caps.version = Some(v.clone());
            }
            Event::Reply(Reply::Mode { mode: 1016, state }) => caps.pixel_mouse = state.is_supported(),
            Event::Reply(Reply::Mode { mode: 2026, state }) => caps.sync_output = state.is_supported(),
            Event::Reply(Reply::KeyboardFlags(f)) => caps.keyboard_flags = Some(*f),
            Event::Reply(Reply::CellSize { width, height }) if *width > 0 && *height > 0 => {
                reported_cell = Some(CellSize { width: *width, height: *height })
            }
            _ => {}
        }
    }
    // The environment may not say (e.g. a multiplexer on the far side of SSH),
    // but the multiplexer itself answers XTVERSION.
    caps.in_zellij |= caps.terminal == TerminalKind::Zellij;
    caps.in_tmux |= caps.terminal == TerminalKind::Tmux;
    // WezTerm answers DECRQM 2026 with "reset" although it implements it.
    if caps.terminal == TerminalKind::WezTerm {
        caps.sync_output = true;
    }
    caps.geometry.cell = kernel.map(|g| g.cell).or(reported_cell).unwrap_or(FALLBACK_CELL);
    caps
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::Parser;

    fn replies(bytes: &[u8]) -> Vec<Event> {
        Parser::new().feed(bytes)
    }

    #[test]
    fn kitty_with_everything() {
        let r = replies(
            b"\x1bP>|kitty(0.49.2)\x1b\\\x1b_Gi=8257537;OK\x1b\\\x1b_Gi=8257538;OK\x1b\\\x1b_Gi=8257539;OK\x1b\\\
              \x1b[?1016;2$y\x1b[?2026;2$y\x1b[?0u\x1b[6;20;10t\x1b[?62;52c",
        );
        let caps = interpret(&Env::default(), &r, None, (80, 24));
        assert_eq!(caps.terminal, TerminalKind::Kitty);
        assert!(caps.kitty_graphics && caps.shm && caps.temp_file && caps.pixel_mouse && caps.sync_output);
        assert_eq!(caps.keyboard_flags, Some(0));
        assert_eq!(caps.geometry.cell, CellSize { width: 10, height: 20 });
        assert_eq!(caps.backend(), Backend::Layered);
        assert_eq!(caps.medium(), Medium::SharedMemory);
    }

    #[test]
    fn remote_kitty_falls_back_to_direct() {
        let r = replies(
            b"\x1b_Gi=8257537;OK\x1b\\\x1b_Gi=8257538;ENOENT:no shm\x1b\\\x1b_Gi=8257539;EBADF:x\x1b\\\x1b[?62c",
        );
        let caps = interpret(&Env::default(), &r, None, (80, 24));
        assert!(caps.kitty_graphics && !caps.shm && !caps.temp_file);
        assert_eq!(caps.medium(), Medium::Direct { compress: true });
    }

    #[test]
    fn no_graphics_when_only_da1() {
        let caps = interpret(&Env::default(), &replies(b"\x1b[?62;22c"), None, (80, 24));
        assert!(!caps.kitty_graphics);
        assert_eq!(caps.geometry.cell, FALLBACK_CELL);
    }

    #[test]
    fn wezterm_uses_composited_and_sync() {
        let r = replies(b"\x1bP>|WezTerm 20240203\x1b\\\x1b_Gi=8257537;OK\x1b\\\x1b[?2026;0$y\x1b[?62c");
        let caps = interpret(&Env::default(), &r, None, (80, 24));
        assert_eq!(caps.terminal, TerminalKind::WezTerm);
        assert!(caps.sync_output);
        assert!(matches!(caps.backend(), Backend::Composited { .. }));
    }

    #[test]
    fn tmux_settings() {
        let t = Tmux::from_outputs("", "on\n", "kitty(0.49.2)\n", "xterm-kitty\n");
        assert!(t.passthrough && t.outer_supports_placeholders());
        assert_eq!(t.outer, "kitty(0.49.2)");
        // The pane value wins over the global one; termname is the fallback.
        let t = Tmux::from_outputs("off", "all", "", "xterm-ghostty");
        assert!(!t.passthrough && t.outer_supports_placeholders());
        assert!(!Tmux::from_outputs("", "on", "WezTerm 2024", "").outer_supports_placeholders());
        let caps = interpret(&Env { tmux: true, ..Default::default() }, &replies(b"\x1b[?1;2c"), None, (80, 24));
        assert_eq!(caps.backend(), Backend::placeholders(2));
    }

    #[test]
    fn zellij_is_recognised_from_xtversion_alone() {
        let r = replies(b"\x1bP>|Zellij(4501)\x1b\\\x1b_Gi=8257537;OK\x1b\\\x1b[?2026;2$y\x1b[?1u\x1b[?62;52c");
        let caps = interpret(&Env::default(), &r, None, (80, 24));
        assert_eq!(caps.terminal, TerminalKind::Zellij);
        assert!(caps.in_zellij && !caps.pixel_mouse);
        assert!(matches!(caps.backend(), Backend::Composited { .. }));
    }

    #[test]
    fn overrides_and_kernel_cell_size() {
        let env = Env {
            backend_override: Some("composited".into()),
            medium_override: Some("direct".into()),
            ..Default::default()
        };
        let kernel = Geometry { cols: 100, rows: 30, cell: CellSize { width: 9, height: 18 } };
        let caps = interpret(&env, &replies(b"\x1b[6;20;10t\x1b[?62c"), Some(kernel), (100, 30));
        assert_eq!(caps.geometry.cell, CellSize { width: 9, height: 18 });
        assert!(matches!(caps.backend(), Backend::Composited { .. }));
        assert_eq!(caps.medium(), Medium::Direct { compress: true });
    }
}
