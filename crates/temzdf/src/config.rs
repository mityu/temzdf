//! Where the configuration script lives: `$XDG_CONFIG_HOME/temzdf/config.ts`
//! or `config.js` (see doc/config.md), and the settings a session gets.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use temzdf_server::session::{ScriptSettings, Settings};

/// `$XDG_CONFIG_HOME/temzdf` (falling back to `~/.config/temzdf`). A
/// relative `$XDG_CONFIG_HOME` is ignored, as the XDG specification says
/// (the script host reads absolute paths only).
fn config_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| std::path::Path::new(v).is_absolute())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("temzdf"))
}

/// The configuration script, `config.ts` or `config.js` in the configuration
/// directory (see doc/design/script-host.md). Both at once is an error.
pub fn script_path() -> Result<Option<PathBuf>> {
    let Some(dir) = config_dir() else { return Ok(None) };
    let found: Vec<PathBuf> = ["config.ts", "config.js"].iter().map(|n| dir.join(n)).filter(|p| p.is_file()).collect();
    match found.as_slice() {
        [] => Ok(None),
        [one] => Ok(Some(one.clone())),
        _ => bail!("both config.ts and config.js in {}: keep one", dir.display()),
    }
}

/// How the session runs the script host: the shipped defaults, then the
/// configuration (`explicit`, else `config.ts`/`config.js`) unless `clean`.
pub fn script_settings(clean: bool, explicit: Option<&Path>) -> Result<Option<ScriptSettings>> {
    let config = match (clean, explicit) {
        (true, _) => None,
        (false, Some(p)) if !p.is_file() => bail!("no configuration file {}", p.display()),
        (false, Some(p)) => Some(std::path::absolute(p)?),
        (false, None) => script_path()?,
    };
    Ok(Some(ScriptSettings { config }))
}

/// Session settings.
pub fn settings(serve_forward: bool, script: Option<ScriptSettings>) -> Settings {
    Settings { serve_forward, script }
}
