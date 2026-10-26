mod app;
mod client;
mod config;
mod headless;

use temzdf_server::forward as ipc;

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use termpix::geom::{CellSize, Geometry};

/// Terminal PDF viewer drawn with the kitty graphics protocol.
///
/// `temzdf FILE` shows FILE in this terminal. With `-s NAME` it runs in a
/// session that survives detaching (`C-s d`) and can be attached again from
/// anywhere (`temzdf attach NAME`); without FILE the session starts with a
/// shell. The session commands follow zellij's names.
#[derive(Parser, Debug)]
#[command(version, args_conflicts_with_subcommands = true)]
struct Cli {
    #[command(subcommand)]
    command: Option<Cmd>,
    /// PDF file to open.
    file: Option<PathBuf>,
    /// Start a new session called NAME showing FILE (a shell without FILE;
    /// named after the file when NAME is the file: `temzdf -s doc.pdf`) and
    /// attach to it.
    #[arg(short, long, value_name = "NAME")]
    session: Option<String>,
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
    /// For --render-frame: keys to press first, in Vim's feedkeys notation, e.g.
    /// "<Tab>jj<CR>" (see headless.rs).
    #[arg(long, value_name = "KEYS")]
    keys: Option<String>,
    /// For --render-frame: blend translucent layers in linear light like kitty
    /// (scripts/e2e.sh compares against real terminals).
    #[arg(long, hide = true)]
    linear_blending: bool,
}

#[derive(Subcommand, Debug)]
enum Cmd {
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
    /// Run an action in a session: new-pane [FILE | -- COMMAND...],
    /// split right|down, move-focus left|right|up|down, focus-next-pane,
    /// close-pane, detach.
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
    /// Run the configuration script for a session (started by the session).
    #[command(hide = true)]
    ScriptHost,
    /// Print TypeScript declarations of the scripting API (temzdf.d.ts).
    #[command(hide = true)]
    PrintTypes,
    /// Run a session server (started by `temzdf -s`).
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

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Some(Cmd::ListSessions { verbose }) => return client::list_sessions(verbose),
        Some(Cmd::KillSession { name }) => return client::kill_session(&name),
        Some(Cmd::KillAllSessions) => return client::kill_all_sessions(),
        Some(Cmd::Action { session, timeout, action }) => {
            let timeout = std::time::Duration::try_from_secs_f64(timeout.max(0.001))
                .map_err(|_| anyhow::anyhow!("--timeout {timeout}: not a number of seconds"))?;
            return client::action(&client::resolve(session.as_deref())?, action, timeout);
        }
        Some(Cmd::PrintTypes) => {
            print!("{}", temzdf_script::types::declarations(&temzdf_server::session::command_specs()));
            return Ok(());
        }
        Some(Cmd::ScriptHost) => {
            return temzdf_script::run(std::io::stdin(), std::io::stdout()).map_err(anyhow::Error::msg);
        }
        Some(Cmd::Attach { name, create, file }) => {
            let name = match (name, create, file) {
                (Some(n), true, file) if temzdf_server::daemon::list()?.iter().all(|s| s.name != n) => {
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
    let page = cli.page.map(|p| p.saturating_sub(1));
    if let Some(name) = &cli.session {
        // `temzdf -s doc.pdf`: the name was the file.
        let (name, file) = match &cli.file {
            Some(f) => (name.clone(), Some(f.clone())),
            None if std::path::Path::new(name).is_file() => {
                (client::name_for(name.as_ref())?, Some(PathBuf::from(name)))
            }
            None => (name.clone(), None),
        };
        let pane = client::first_pane(file.as_deref())?;
        client::spawn_server(&name, &pane, cli.config.as_deref(), cli.editor.as_deref(), cli.clean)?;
        println!("{}", client::attach(&name)?);
        return Ok(());
    }
    let Some(file) = cli.file else { bail!("no PDF file given (see --help)") };
    if let Some(out) = &cli.render_frame {
        let (cols, rows) = parse_pair(&cli.size)?;
        let (cw, ch) = parse_pair(&cli.cell)?;
        let geometry = Geometry { cols: cols as u16, rows: rows as u16, cell: CellSize { width: cw, height: ch } };
        let opts = headless::FrameOptions {
            geometry,
            page,
            scale: cli.scale,
            keys: cli.keys.clone(),
            linear_blending: cli.linear_blending,
        };
        return headless::render_frame(&file, out, &opts);
    }
    let forward = match &cli.forward {
        Some(spec) => Some(ipc::ForwardRequest::parse_spec(spec).with_context(|| format!("bad --forward {spec:?}"))?),
        None => None,
    };
    // A viewer of this file is already running: hand the request over and exit.
    if let Some(req) = &forward
        && ipc::send_forward(&file, req)?
    {
        return Ok(());
    }
    let config = cli.config.clone();
    app::run(
        &file,
        app::Options { page, scale: cli.scale, config, forward, editor: cli.editor.clone(), clean: cli.clean },
    )
}
