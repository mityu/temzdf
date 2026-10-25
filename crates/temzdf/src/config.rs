//! Where the configuration script lives: `$XDG_CONFIG_HOME/temzdf/config.ts`
//! or `config.js` (see doc/config.md).

use std::path::PathBuf;

use anyhow::Result;

/// `$XDG_CONFIG_HOME/temzdf` (falling back to `~/.config/temzdf`).
fn config_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("temzdf"))
}

/// The configuration script, `config.ts` or `config.js` in the configuration
/// directory (see doc/script-host.md). Both at once is an error.
pub fn script_path() -> Result<Option<PathBuf>> {
    let Some(dir) = config_dir() else { return Ok(None) };
    let found: Vec<PathBuf> = ["config.ts", "config.js"].iter().map(|n| dir.join(n)).filter(|p| p.is_file()).collect();
    match found.as_slice() {
        [] => Ok(None),
        [one] => Ok(Some(one.clone())),
        _ => anyhow::bail!("both config.ts and config.js in {}: keep one", dir.display()),
    }
}
