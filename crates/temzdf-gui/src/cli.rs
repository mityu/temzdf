//! The command line of `temzdf-gui`.

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use temzdf_scene::FontEntry;

/// Show a temzdf session in a window: session NAME (started with FILE if it
/// does not exist), a new session showing FILE, or the only session (a new
/// shell when there is none). Closing the window detaches.
#[derive(Parser, Debug)]
#[command(version, args_conflicts_with_subcommands = true)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
    /// PDF file for a new session.
    pub file: Option<PathBuf>,
    /// The session to show.
    #[arg(short, long, value_name = "NAME")]
    pub session: Option<String>,
    /// A session started here does not run the configuration script
    /// ($XDG_CONFIG_HOME/temzdf/config.ts or config.js), as `temzdf --clean`.
    #[arg(long)]
    pub clean: bool,
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

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Show which fonts the GUI uses for a font list (`temzdf ls-fonts` passes
    /// the configured one).
    LsFonts(LsFonts),
}

#[derive(clap::Args, Debug)]
pub struct LsFonts {
    /// A font of the list, first choice first (repeat for more): NAME or
    /// NAME:SIZE in points. Default: the built-in JetBrains Mono.
    #[arg(long = "font", value_name = "NAME[:SIZE]")]
    pub font: Vec<String>,
    /// A font tried first for wide characters, as --font.
    #[arg(long = "font-wide", value_name = "NAME[:SIZE]")]
    pub font_wide: Vec<String>,
    /// Show which font draws each character of TEXT.
    #[arg(long)]
    pub text: Option<String>,
    /// List every font available, installed or built in.
    #[arg(long)]
    pub list_system: bool,
}

/// A `--font` value: NAME or NAME:SIZE.
pub fn font_entry(arg: &str) -> FontEntry {
    match arg.rsplit_once(':') {
        Some((name, size)) if size.trim().parse::<f32>().is_ok_and(|n| n > 0.0) => {
            FontEntry { family: name.trim().into(), size: size.trim().parse().ok() }
        }
        _ => FontEntry { family: arg.trim().into(), size: None },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn font_arguments_name_a_size_after_a_colon() {
        let e = |f: &str, size| FontEntry { family: f.into(), size };
        assert_eq!(font_entry("UDEV Gothic NF:13.5"), e("UDEV Gothic NF", Some(13.5)));
        assert_eq!(font_entry("Noto Sans CJK JP"), e("Noto Sans CJK JP", None));
        assert_eq!(font_entry("Odd:Name"), e("Odd:Name", None), "not a size");
    }
}
