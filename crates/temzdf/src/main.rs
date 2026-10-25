mod app;
mod config;
mod headless;
mod ipc;
mod keys;
mod layout;
mod outline;
mod pdf;
mod present;
mod search;
mod selection;
mod session;
mod synctex;
mod viewer;
mod workers;

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use termpix::geom::{CellSize, Geometry};

/// Terminal PDF viewer drawn with the kitty graphics protocol.
#[derive(Parser, Debug)]
#[command(version)]
struct Cli {
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
    /// SyncTeX inverse search command (Ctrl+click), with %{input}, %{line} and
    /// %{column} placeholders. Overrides [synctex] editor in the config file.
    #[arg(long, value_name = "COMMAND")]
    editor: Option<String>,
    /// Configuration file (default: $XDG_CONFIG_HOME/temzdf/config.toml).
    #[arg(long, value_name = "FILE")]
    config: Option<PathBuf>,
    /// For --render-frame: keys to press first, e.g. "Tab j Enter" (see headless.rs).
    #[arg(long, value_name = "KEYS")]
    keys: Option<String>,
    /// For --render-frame: blend translucent layers in linear light like kitty
    /// (scripts/e2e.sh compares against real terminals).
    #[arg(long, hide = true)]
    linear_blending: bool,
}

fn parse_pair(s: &str) -> Result<(u32, u32)> {
    let (a, b) = s.split_once('x').with_context(|| format!("expected AxB, got {s:?}"))?;
    Ok((a.parse()?, b.parse()?))
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let page = cli.page.map(|p| p.saturating_sub(1));
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
        return headless::render_frame(&cli.file, out, &opts);
    }
    let forward = match &cli.forward {
        Some(spec) => Some(ipc::ForwardRequest::parse_spec(spec).with_context(|| format!("bad --forward {spec:?}"))?),
        None => None,
    };
    // A viewer of this file is already running: hand the request over and exit.
    if let Some(req) = &forward
        && ipc::send_forward(&cli.file, req)?
    {
        return Ok(());
    }
    let config = config::Config::load(cli.config.as_deref())?;
    app::run(&cli.file, app::Options { page, scale: cli.scale, config, forward, editor: cli.editor.clone() })
}
