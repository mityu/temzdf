mod app;
mod client;
mod config;
mod headless;
mod lsfonts;
mod snapshot;

use temzdf_server::forward as ipc;

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use termpix::geom::{CellSize, Geometry};

/// Terminal PDF viewer drawn with the kitty graphics protocol, in sessions.
///
/// `temzdf [FILE]` starts a new session showing FILE (a shell without FILE),
/// named alfa, bravo, ... unless `-s NAME` names it, and attaches to it. A
/// session survives detaching (`C-s d`) and can be attached again from
/// anywhere (`temzdf attach NAME`). `temzdf standalone pdf FILE` shows FILE in
/// this terminal without a session. The session commands follow zellij's
/// names.
#[derive(Parser, Debug)]
#[command(version, args_conflicts_with_subcommands = true)]
struct Cli {
    #[command(subcommand)]
    command: Option<Cmd>,
    /// PDF file to open.
    file: Option<PathBuf>,
    /// Call the new session NAME (named after the file when NAME is the file:
    /// `temzdf -s doc.pdf`).
    #[arg(short, long, value_name = "NAME")]
    session: Option<String>,
    /// SyncTeX forward search: show what LINE[:COLUMN]:FILE produced. Sent to the
    /// viewer or session that already has the PDF open, or opens it in a new
    /// session.
    #[arg(long, value_name = "LINE[:COLUMN]:FILE", requires = "file")]
    forward: Option<String>,
    /// SyncTeX inverse search command (Ctrl+click), with %{input}, %{line} and
    /// %{column} placeholders. Overrides `o.synctexeditor` of the configuration.
    #[arg(long, value_name = "COMMAND", global = true)]
    editor: Option<String>,
    /// Configuration script (default: $XDG_CONFIG_HOME/temzdf/config.ts or
    /// config.js).
    #[arg(long, value_name = "FILE", global = true)]
    config: Option<PathBuf>,
    /// Do not run the configuration script ($XDG_CONFIG_HOME/temzdf/config.ts
    /// or config.js).
    #[arg(long, global = true)]
    clean: bool,
}

/// What `temzdf standalone pdf` takes.
#[derive(clap::Args, Debug)]
struct PdfArgs {
    /// PDF file to open.
    file: PathBuf,
    /// Render one screen to this PNG instead of starting the viewer.
    #[arg(long, value_name = "PNG")]
    render_frame: Option<PathBuf>,
    /// Screen size for --render-frame, in cells (COLSxROWS).
    #[arg(long, default_value = "100x30", value_name = "COLSxROWS")]
    size: String,
    /// Cell size for --render-frame, in pixels (WxH).
    #[arg(long, default_value = "10x20", value_name = "WxH")]
    cell: String,
    /// Page to show first (1-based).
    #[arg(long)]
    page: Option<usize>,
    /// Zoom in pixels per PDF point (default: fit width).
    #[arg(long)]
    scale: Option<f32>,
    /// SyncTeX forward search: show what LINE[:COLUMN]:FILE produced. Sent to the
    /// viewer that already has the PDF open, or opens it.
    #[arg(long, value_name = "LINE[:COLUMN]:FILE")]
    forward: Option<String>,
    /// For --render-frame: keys to press first, in Vim's feedkeys notation, e.g.
    /// "<Tab>jj<CR>" (see headless.rs).
    #[arg(long, value_name = "KEYS", conflicts_with = "events")]
    keys: Option<String>,
    /// For --render-frame: a timeline of keys and mouse input to replay first
    /// (one per line: "MS key KEYS", "MS press|release|drag X Y BUTTON",
    /// "MS move X Y", "MS wheel DIR X Y", "MS end"; see headless.rs).
    #[arg(long, value_name = "FILE")]
    events: Option<PathBuf>,
    /// For --render-frame: blend translucent layers in linear light like kitty
    /// (scripts/e2e.sh compares against real terminals).
    #[arg(long, hide = true)]
    linear_blending: bool,
}

/// Viewers that run in this terminal, without a session.
#[derive(Subcommand, Debug)]
enum Standalone {
    /// The PDF viewer.
    Pdf(PdfArgs),
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Run a viewer in this terminal, without a session.
    Standalone {
        #[command(subcommand)]
        kind: Standalone,
    },
    /// Attach to a session (the only one when NAME is left out).
    #[command(alias = "a")]
    Attach {
        name: Option<String>,
        /// Create the session NAME (with FILE, or a shell) when it does not exist.
        #[arg(short, long, requires = "name")]
        create: bool,
        file: Option<PathBuf>,
    },
    /// List the sessions.
    #[command(alias = "ls")]
    ListSessions {
        /// Also show each server's pid and log file.
        #[arg(short, long)]
        verbose: bool,
    },
    /// End a session.
    #[command(alias = "k")]
    KillSession { name: String },
    /// End every session.
    #[command(alias = "ka")]
    KillAllSessions,
    /// Run a command in a session: any command of the registry (`temzdf
    /// print-types` lists them), e.g. new-pane [FILE | -- COMMAND...], split
    /// right|down, move-focus left|right|up|down, close-pane, detach.
    Action {
        /// The session (the only one when left out).
        #[arg(short, long, value_name = "NAME")]
        session: Option<String>,
        /// Give up waiting for the session after this long.
        #[arg(long, value_name = "SECONDS", default_value_t = 10.0)]
        timeout: f64,
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
        action: Vec<String>,
    },
    /// Show the fonts GUI clients use (as `wezterm ls-fonts`): those of the
    /// configuration, of session NAME, or of the session this runs in.
    LsFonts {
        /// The session whose fonts to show.
        #[arg(short, long, value_name = "NAME")]
        session: Option<String>,
        /// Show this font list instead (NAME or NAME:SIZE, repeated).
        #[arg(long = "font", value_name = "NAME[:SIZE]")]
        font: Vec<String>,
        /// The fonts tried first for wide characters, as --font.
        #[arg(long = "font-wide", value_name = "NAME[:SIZE]")]
        font_wide: Vec<String>,
        /// Show which font draws each character of TEXT.
        #[arg(long)]
        text: Option<String>,
        /// List every font available, installed or built in.
        #[arg(long)]
        list_system: bool,
    },
    /// Run the configuration script for a session (started by the session).
    #[command(hide = true)]
    ScriptHost,
    /// Draw session NAME's frame as a terminal client of this geometry would
    /// show it, into a PNG (scripts/e2e.sh compares terminals against it).
    #[command(hide = true)]
    RenderSession {
        name: String,
        out: PathBuf,
        /// The screen in cells (COLSxROWS).
        #[arg(long, value_name = "COLSxROWS")]
        size: String,
        /// The cell size in pixels (WxH).
        #[arg(long, value_name = "WxH")]
        cell: String,
        /// The terminal client's hints (from its backend and medium).
        #[arg(long)]
        exact_scaling: bool,
        #[arg(long)]
        fast_uploads: bool,
        /// Blend translucent layers in linear light like kitty.
        #[arg(long)]
        linear_blending: bool,
        /// Write the text of the screen here.
        #[arg(long, value_name = "FILE")]
        text: Option<PathBuf>,
        /// Write a mask of the cells the terminal draws itself here.
        #[arg(long, value_name = "PNG")]
        mask: Option<PathBuf>,
    },
    /// Print TypeScript declarations of the scripting API (temzdf.d.ts).
    #[command(hide = true)]
    PrintTypes,
    /// Run a session server (started when `temzdf`, `attach -c` or
    /// `--forward` starts a session).
    #[command(hide = true)]
    Server {
        name: String,
        /// Where the server's output goes (for `temzdf ls`).
        #[arg(long, value_name = "FILE")]
        log: Option<PathBuf>,
        /// What the first pane shows, as `new-pane` takes it.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        pane: Vec<String>,
    },
}

fn parse_pair(s: &str) -> Result<(u32, u32)> {
    let (a, b) = s.split_once('x').with_context(|| format!("expected AxB, got {s:?}"))?;
    Ok((a.parse()?, b.parse()?))
}

/// Options that apply only when a session starts, given (`file`: a FILE).
fn session_only_options(cli: &Cli, file: bool) -> Vec<&'static str> {
    [(file, "FILE"), (cli.editor.is_some(), "--editor"), (cli.config.is_some(), "--config"), (cli.clean, "--clean")]
        .into_iter()
        .filter_map(|(given, name)| given.then_some(name))
        .collect()
}

/// Say which given options `by` (a command) does not use: they are global
/// in clap, so nothing refuses them.
fn warn_unused(options: &[&str], by: &str) {
    for o in options {
        eprintln!("temzdf: {o} is not used by {by}");
    }
}

fn main() -> Result<()> {
    let mut cli = Cli::parse();
    // Flags for starting a session, for warnings when it exists already.
    let start_flags = session_only_options(&cli, false);
    match cli.command.take() {
        Some(Cmd::Standalone { kind: Standalone::Pdf(args) }) => {
            // A frame is drawn with the shipped configuration, and no
            // viewer runs to search.
            if args.render_frame.is_some() {
                let forward = args.forward.is_some().then_some("--forward");
                warn_unused(&start_flags.iter().copied().chain(forward).collect::<Vec<_>>(), "--render-frame");
            }
            return standalone_pdf(args, &cli);
        }
        Some(Cmd::ListSessions { verbose }) => {
            warn_unused(&start_flags, "list-sessions");
            return client::list_sessions(verbose);
        }
        Some(Cmd::KillSession { name }) => {
            warn_unused(&start_flags, "kill-session");
            return client::kill_session(&name);
        }
        Some(Cmd::KillAllSessions) => {
            warn_unused(&start_flags, "kill-all-sessions");
            return client::kill_all_sessions();
        }
        Some(Cmd::Action { session, timeout, action }) => {
            warn_unused(&start_flags, "action");
            let timeout = std::time::Duration::try_from_secs_f64(timeout.max(0.001))
                .map_err(|_| anyhow::anyhow!("--timeout {timeout}: not a number of seconds"))?;
            // In a session's pane, that session (as zellij does).
            let session = session.or_else(|| std::env::var("TEMZDF_SESSION_NAME").ok());
            return client::action(&client::resolve(session.as_deref())?, action, timeout);
        }
        Some(Cmd::LsFonts { session, font, font_wide, text, list_system }) => {
            let looks = if !font.is_empty() || !font_wide.is_empty() {
                temzdf_scene::Appearance::default()
            } else if let Some(name) = session.or_else(|| std::env::var("TEMZDF_SESSION_NAME").ok()) {
                lsfonts::from_session(&name)?
            } else {
                lsfonts::from_configuration(cli.clean, cli.config.as_deref())?
            };
            let mut rest = Vec::new();
            for f in font {
                rest.extend(["--font".to_owned(), f]);
            }
            for f in font_wide {
                rest.extend(["--font-wide".to_owned(), f]);
            }
            if let Some(t) = text {
                rest.extend(["--text".to_owned(), t]);
            }
            if list_system {
                rest.push("--list-system".into());
            }
            return lsfonts::run(&looks, &rest);
        }
        Some(Cmd::PrintTypes) => {
            print!("{}", temzdf_script::types::declarations(&temzdf_server::session::command_specs()));
            return Ok(());
        }
        Some(Cmd::RenderSession {
            name,
            out,
            size,
            cell,
            exact_scaling,
            fast_uploads,
            linear_blending,
            text,
            mask,
        }) => {
            let (cols, rows) = parse_pair(&size)?;
            let (cw, ch) = parse_pair(&cell)?;
            let opts = snapshot::SnapshotOptions {
                geometry: Geometry { cols: cols as u16, rows: rows as u16, cell: CellSize { width: cw, height: ch } },
                hints: temzdf_scene::Hints { exact_scaling, fast_uploads },
                linear_blending,
                text,
                mask,
            };
            return snapshot::render_session(&name, &out, &opts);
        }
        Some(Cmd::ScriptHost) => {
            return temzdf_script::run(std::io::stdin(), std::io::stdout()).map_err(anyhow::Error::msg);
        }
        Some(Cmd::Attach { name, create, file }) => {
            if file.is_some() && !create {
                bail!("a FILE is for a new session: attach -c NAME FILE");
            }
            let exists = |n: &str| temzdf_server::daemon::list().is_ok_and(|l| l.iter().any(|s| s.name == n));
            if name.is_none() {
                warn_unused(&start_flags, "attach without a NAME (it starts nothing)");
            }
            if let Some(n) = &name
                && exists(n)
            {
                let unused = file.is_some().then_some("FILE").into_iter().chain(start_flags);
                for unused in unused {
                    eprintln!("temzdf: session {n:?} exists: {unused} not used");
                }
            }
            let name = match (name, create, file) {
                (Some(n), true, file) if temzdf_server::daemon::list()?.iter().all(|s| s.name != n) => {
                    client::need_terminal()?;
                    client::spawn_server(
                        &n,
                        &client::first_pane(file.as_deref())?,
                        cli.config.as_deref(),
                        cli.editor.as_deref(),
                        cli.clean,
                    )?;
                    n
                }
                (name, ..) => client::resolve(name.as_deref())?,
            };
            println!("{}", client::attach(&name)?);
            return Ok(());
        }
        Some(Cmd::Server { name, log, pane }) => {
            // Leave the terminal's session, so closing it does not end ours.
            let _ = rustix::process::setsid();
            let words: Vec<String> = std::iter::once("new-pane".to_owned()).chain(pane).collect();
            let temzdf_server::action::Action::NewPane { spec, .. } = temzdf_server::action::Action::parse(&words)?
            else {
                unreachable!("parsed as new-pane")
            };
            let script = app::script_settings(cli.clean, cli.config.as_deref())?;
            return temzdf_server::daemon::run(&name, &spec, log, app::settings(true, script, cli.editor.as_deref()));
        }
        None => {}
    }
    if let Some(spec) = &cli.forward {
        let req = ipc::ForwardRequest::parse_spec(spec).with_context(|| format!("bad --forward {spec:?}"))?;
        let file = cli.file.as_deref().expect("clap requires FILE with --forward");
        // A viewer or a session's pane has the file open: hand it over.
        if ipc::send_forward(file, &req)? {
            return Ok(());
        }
        let name = match &cli.session {
            Some(n) => Naming::Given(n.clone()),
            None => Naming::Auto,
        };
        let file = file.to_owned();
        return start_session(name, Some(&file.clone()), &cli, move || {
            // The pane listens once it has opened the file.
            std::thread::spawn(move || {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
                while std::time::Instant::now() < deadline {
                    if ipc::send_forward(&file, &req).unwrap_or(false) {
                        return;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            });
        });
    }
    let (name, file) = match &cli.session {
        Some(name) => {
            // `-s doc.pdf` names a file (a PDF's name only: `-s build` with a
            // file `build` here is a session named so).
            let pdf = name.to_ascii_lowercase().ends_with(".pdf");
            // `temzdf -s typo.pdf`: not a name, a file that is not there.
            if cli.file.is_none() && pdf && !std::path::Path::new(name).is_file() {
                bail!("{name}: no such file");
            }
            // `temzdf -s doc.pdf`: the name was the file.
            match &cli.file {
                Some(f) => (Naming::Given(name.clone()), Some(f.clone())),
                None if pdf && std::path::Path::new(name).is_file() => {
                    (Naming::AfterFile(PathBuf::from(name)), Some(PathBuf::from(name)))
                }
                None => (Naming::Given(name.clone()), None),
            }
        }
        None => (Naming::Auto, cli.file.clone()),
    };
    start_session(name, file.as_deref(), &cli, || {})
}

/// How a new session is named.
enum Naming {
    /// By the user (`-s NAME`): taken already is an error.
    Given(String),
    /// alfa, bravo, ...: the next one if another server takes it first.
    Auto,
    /// After a file (`-s doc.pdf`): doc, doc-2, ...
    AfterFile(PathBuf),
}

/// Start a session showing `file` (a shell without), run `started` once its
/// server listens, and attach to it.
fn start_session(naming: Naming, file: Option<&std::path::Path>, cli: &Cli, started: impl FnOnce()) -> Result<()> {
    // A missing file first: the more useful error.
    let pane = client::first_pane(file)?;
    client::need_terminal()?;
    let spawn = |n: &str| client::spawn_server(n, &pane, cli.config.as_deref(), cli.editor.as_deref(), cli.clean);
    let name = match &naming {
        Naming::Given(n) => temzdf_client::spawn_named(|| Ok(n.clone()), false, spawn)?,
        Naming::Auto => temzdf_client::spawn_named(client::auto_name, true, spawn)?,
        Naming::AfterFile(f) => temzdf_client::spawn_named(|| client::name_for(f), true, spawn)?,
    };
    started();
    println!("{}", client::attach(&name)?);
    Ok(())
}

/// `temzdf standalone pdf`: the viewer in this terminal, or one frame of it.
fn standalone_pdf(args: PdfArgs, cli: &Cli) -> Result<()> {
    let page = args.page.map(|p| p.saturating_sub(1));
    let file = args.file;
    if let Some(out) = &args.render_frame {
        let (cols, rows) = parse_pair(&args.size)?;
        let (cw, ch) = parse_pair(&args.cell)?;
        let geometry = Geometry { cols: cols as u16, rows: rows as u16, cell: CellSize { width: cw, height: ch } };
        let opts = headless::FrameOptions {
            geometry,
            page,
            scale: args.scale,
            keys: args.keys.clone(),
            events: match &args.events {
                Some(f) => Some(headless::parse_events(
                    &std::fs::read_to_string(f).with_context(|| format!("reading {}", f.display()))?,
                )?),
                None => None,
            },
            linear_blending: args.linear_blending,
        };
        return headless::render_frame(&file, out, &opts);
    }
    let forward = match &args.forward {
        Some(spec) => Some(ipc::ForwardRequest::parse_spec(spec).with_context(|| format!("bad --forward {spec:?}"))?),
        None => None,
    };
    // A viewer of this file is already running: hand the request over and exit.
    if let Some(req) = &forward
        && ipc::send_forward(&file, req)?
    {
        return Ok(());
    }
    app::run(
        &file,
        app::Options {
            page,
            scale: args.scale,
            config: cli.config.clone(),
            forward,
            editor: cli.editor.clone(),
            clean: cli.clean,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_that_do_not_apply_are_named() {
        let cli = |args: &[&str]| Cli::try_parse_from(std::iter::once("temzdf").chain(args.iter().copied())).unwrap();
        // The viewer's options are the viewer's.
        assert!(Cli::try_parse_from(["temzdf", "-s", "x", "--page", "3"]).is_err());
        assert!(Cli::try_parse_from(["temzdf", "standalone", "pdf", "a.pdf", "--page", "3", "--clean"]).is_ok());
        assert_eq!(
            session_only_options(&cli(&["attach", "x", "--clean", "--editor", "vi"]), true),
            ["FILE", "--editor", "--clean"]
        );
        assert!(session_only_options(&cli(&["attach", "x"]), false).is_empty());
    }
}
