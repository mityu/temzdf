//! `temzdf-gui`: show a temzdf session in a window (doc/gui-client.md).

use anyhow::{Context, Result};
use clap::Parser;
use temzdf_gui::cli::Cli;
use temzdf_gui::connection::session_for;

fn main() -> Result<()> {
    let cli = Cli::parse();
    let (name, started) = session_for(cli.session.as_deref(), cli.file.as_deref())?;
    if let Some(out) = &cli.render_frame {
        let (w, h) = cli.size.split_once('x').context("--size is WxH")?;
        let size = (w.parse().context("--size width")?, h.parse().context("--size height")?);
        let result = temzdf_gui::headless::render_frame(&name, out, size, cli.scale);
        // A session started only to be drawn goes again.
        if started {
            let _ = temzdf_gui::connection::kill(&name);
        }
        return result;
    }
    let line = temzdf_gui::app::run(name)?;
    if !line.is_empty() {
        println!("{line}");
    }
    Ok(())
}
