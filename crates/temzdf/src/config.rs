//! User configuration: `$XDG_CONFIG_HOME/temzdf/config.toml` (or `--config`).
//! Every field is optional; unknown keys are errors so typos are noticed. See
//! `doc/config.md` for the documented format.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;
use termpix::compositor::Backend;
use termpix::graphics::{DEFAULT_ZLIB_LEVEL, Medium};

use crate::keys::{Command, Keymap};

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub terminal: TerminalConfig,
    pub scroll: ScrollConfig,
    pub links: LinksConfig,
    pub synctex: SynctexConfig,
    pub reload: ReloadConfig,
    /// Command -> keys, replacing that command's default keys (see `keys.rs`).
    pub keys: HashMap<Command, Vec<String>>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct ReloadConfig {
    /// Wait this long after the last change before reloading.
    pub debounce_ms: u64,
    /// How the reading position is kept across a reload.
    pub anchor: ReloadAnchor,
}

impl Default for ReloadConfig {
    fn default() -> Self {
        Self { debounce_ms: 200, anchor: ReloadAnchor::Position }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ReloadAnchor {
    /// Same page and place on the page (like Skim and Okular).
    #[default]
    Position,
    /// The line of text being read, found again in the new version.
    Text,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct SynctexConfig {
    /// Inverse search command, run with `sh -c`. `%{input}`, `%{line}` and
    /// `%{column}` are replaced by the (shell-quoted) source file, line and column.
    pub editor: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum BackendChoice {
    #[default]
    Auto,
    Layered,
    Composited,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MediumChoice {
    #[default]
    Auto,
    Direct,
    Shm,
    File,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct TerminalConfig {
    /// How frames are sent; `auto` follows detection.
    pub backend: BackendChoice,
    /// How pixels are transferred; `auto` follows detection.
    pub medium: MediumChoice,
    /// zlib level for inline (direct) transfers, 0-9. Level 1 crashes Ghostty 1.3.1.
    pub zlib_level: u32,
}

impl Default for TerminalConfig {
    fn default() -> Self {
        Self { backend: BackendChoice::Auto, medium: MediumChoice::Auto, zlib_level: DEFAULT_ZLIB_LEVEL }
    }
}

impl TerminalConfig {
    /// The backend to use given what detection chose. Environment overrides
    /// (`TERMPIX_BACKEND`) are already part of `detected` and win.
    pub fn backend(&self, detected: Backend, env_override: bool) -> Backend {
        match (env_override, self.backend) {
            (true, _) | (false, BackendChoice::Auto) => detected,
            (false, BackendChoice::Layered) => Backend::Layered,
            (false, BackendChoice::Composited) => Backend::composited(),
        }
    }

    pub fn medium(&self, detected: Medium, env_override: bool) -> Medium {
        match (env_override, self.medium) {
            (true, _) | (false, MediumChoice::Auto) => detected,
            (false, MediumChoice::Direct) => Medium::Direct { compress: true },
            (false, MediumChoice::Shm) => Medium::SharedMemory,
            (false, MediumChoice::File) => Medium::TempFile,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct ScrollConfig {
    /// Pixels per wheel step (before flick acceleration).
    pub step: f64,
    /// Easing time constant in milliseconds; smaller is snappier.
    pub smoothness_ms: u64,
}

impl Default for ScrollConfig {
    fn default() -> Self {
        let d = termpix::scroll::ScrollConfig::default();
        Self { step: d.step, smoothness_ms: d.tau.as_millis() as u64 }
    }
}

impl ScrollConfig {
    pub fn to_termpix(&self) -> termpix::scroll::ScrollConfig {
        termpix::scroll::ScrollConfig {
            step: self.step.max(1.0),
            tau: std::time::Duration::from_millis(self.smoothness_ms.max(1)),
            ..Default::default()
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct LinksConfig {
    /// Command used to open external links; the URI is appended as the last
    /// argument. Default: `xdg-open` (Linux) or `open` (macOS).
    pub open_command: Option<Vec<String>>,
    /// Preview internal links when the pointer rests on them.
    pub preview: bool,
    /// How long the pointer rests on a link before its preview opens.
    pub preview_delay_ms: u64,
}

impl Default for LinksConfig {
    fn default() -> Self {
        Self {
            open_command: None,
            preview: true,
            preview_delay_ms: crate::pane::DEFAULT_PREVIEW_DELAY.as_millis() as u64,
        }
    }
}

impl LinksConfig {
    /// The link preview delay, or `None` when previews are off.
    pub fn preview_delay(&self) -> Option<std::time::Duration> {
        self.preview.then(|| std::time::Duration::from_millis(self.preview_delay_ms))
    }

    pub fn command(&self) -> Vec<String> {
        self.open_command
            .clone()
            .filter(|c| !c.is_empty())
            .unwrap_or_else(|| vec![if cfg!(target_os = "macos") { "open" } else { "xdg-open" }.to_owned()])
    }
}

/// `$XDG_CONFIG_HOME/temzdf/config.toml`, falling back to `~/.config`.
pub fn default_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("temzdf").join("config.toml"))
}

impl Config {
    pub fn parse(text: &str) -> Result<Self> {
        let config: Self = toml::from_str(text)?;
        config.keymap().context("[keys]")?;
        Ok(config)
    }

    pub fn keymap(&self) -> Result<Keymap> {
        Keymap::new(&self.keys)
    }

    /// Load `path`, or the default location. A missing default file is fine; a
    /// missing explicit file is an error.
    pub fn load(path: Option<&Path>) -> Result<Self> {
        let (path, explicit) = match path {
            Some(p) => (p.to_owned(), true),
            None => match default_path() {
                Some(p) => (p, false),
                None => return Ok(Self::default()),
            },
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => Self::parse(&text).with_context(|| format!("in {}", path.display())),
            Err(e) if !explicit && e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("cannot read {}", path.display())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_config_is_default() {
        assert_eq!(Config::parse("").unwrap(), Config::default());
        assert_eq!(Config::default().terminal.zlib_level, DEFAULT_ZLIB_LEVEL);
    }

    #[test]
    fn full_config() {
        let c = Config::parse(
            r#"
            [terminal]
            backend = "composited"
            medium = "direct"
            zlib_level = 6
            [scroll]
            step = 40
            smoothness_ms = 90
            [links]
            open_command = ["firefox", "--new-tab"]
            preview_delay_ms = 500
            [reload]
            debounce_ms = 500
            anchor = "text"
            "#,
        )
        .unwrap();
        assert!(matches!(c.terminal.backend(Backend::Layered, false), Backend::Composited { .. }));
        assert_eq!(c.terminal.backend(Backend::Layered, true), Backend::Layered, "environment wins");
        assert_eq!(c.terminal.medium(Medium::SharedMemory, false), Medium::Direct { compress: true });
        assert_eq!(c.terminal.zlib_level, 6);
        assert_eq!(c.scroll.to_termpix().step, 40.0);
        assert_eq!(c.links.command(), ["firefox", "--new-tab"]);
        assert_eq!(c.links.preview_delay(), Some(std::time::Duration::from_millis(500)));
        assert_eq!(Config::parse("[links]\npreview = false\n").unwrap().links.preview_delay(), None);
        assert_eq!(c.reload, ReloadConfig { debounce_ms: 500, anchor: ReloadAnchor::Text });
    }

    #[test]
    fn keys_section_rebinds_and_reports_mistakes() {
        use termpix::input::{KeyCode, KeyEvent, Modifiers};
        let c = Config::parse("[keys]\nquit = [\"Q\", \"C-q\"]\nnext_match = []\n").unwrap();
        let m = c.keymap().unwrap();
        assert_eq!(m.get(&KeyEvent::new(KeyCode::Char('q'), Modifiers::CTRL)), Some(Command::Quit));
        assert_eq!(m.get(&KeyEvent::new(KeyCode::Char('q'), Modifiers::NONE)), None);
        assert_eq!(m.get(&KeyEvent::new(KeyCode::Char('n'), Modifiers::NONE)), None);
        let unknown_command = Config::parse("[keys]\nexplode = [\"x\"]\n").unwrap_err();
        assert!(format!("{unknown_command:#}").contains("explode"), "{unknown_command:#}");
        let bad_key = Config::parse("[keys]\nquit = [\"C-Nope\"]\n").unwrap_err();
        assert!(format!("{bad_key:#}").contains("Nope"), "{bad_key:#}");
    }

    #[test]
    fn typos_are_errors() {
        let err = Config::parse("[scroll]\nstpe = 3\n").unwrap_err().to_string();
        assert!(err.contains("stpe"), "{err}");
        assert!(Config::parse("[terminal]\nbackend = \"fast\"\n").is_err());
    }
}
