//! The command line of `temzdf-gui`.

use std::path::PathBuf;

use clap::Parser;

/// Show a temzdf session in a window: session NAME (started with FILE if it
/// does not exist), a new session showing FILE, or the only session (a new
/// shell when there is none). Closing the window detaches.
#[derive(Parser, Debug)]
#[command(version)]
pub struct Cli {
    /// PDF file for a new session.
    pub file: Option<PathBuf>,
    /// The session to show.
    #[arg(short, long, value_name = "NAME")]
    pub session: Option<String>,
    /// Draw the session offscreen into this PNG instead of opening a window.
    #[arg(long, value_name = "PNG")]
    pub render_frame: Option<PathBuf>,
    /// For --render-frame: the window's size in pixels.
    #[arg(long, default_value = "900x600", value_name = "WxH")]
    pub size: String,
    /// For --render-frame: the display's scale factor.
    #[arg(long, default_value_t = 1.0)]
    pub scale: f64,
}
