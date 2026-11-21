//! Which session the window shows, and the `temzdf` that starts it; the
//! connection itself is `temzdf_client::Connection`.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use temzdf_protocol as proto;
use temzdf_scene::Hints;

/// The window's frontend hints, sent in its `Hello`.
pub const HINTS: Hints = Hints { exact_scaling: true, fast_uploads: true };

/// The `temzdf` executable that runs session servers: next to this one,
/// else on `PATH`.
pub fn temzdf_exe() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|d| d.join("temzdf")))
        .filter(|p| p.exists())
        .unwrap_or_else(|| PathBuf::from("temzdf"))
}

/// The session to show: `name` (started with `file` if it does not
/// exist), else a new one for `file`, else the only one, else a new shell.
/// Returns its name and whether it was started here; one started here runs
/// no configuration script if `clean`.
pub fn session_for(name: Option<&str>, file: Option<&Path>, clean: bool) -> Result<(String, bool)> {
    let exists = |n: &str| proto::discovery::list().is_ok_and(|l| l.iter().any(|s| s.name == n));
    // A name of its own (`-s`) taken meanwhile is an error; one picked here
    // is picked again.
    let start = |choose: &dyn Fn() -> Result<String>, retry: bool| -> Result<(String, bool)> {
        let pane = temzdf_client::first_pane(file)?;
        let spawn = |n: &str| temzdf_client::spawn_server(&temzdf_exe(), n, &pane, None, None, clean);
        Ok((temzdf_client::spawn_named(choose, retry, spawn)?, true))
    };
    match (name, file) {
        (Some(n), _) if exists(n) => Ok((n.to_owned(), false)),
        (Some(n), _) => start(&|| Ok(n.to_owned()), false),
        // Named as `temzdf [FILE]` names sessions: alfa, bravo, ...
        (None, Some(_)) => start(&temzdf_client::auto_name, true),
        (None, None) => match proto::discovery::list()?.as_slice() {
            [one] => Ok((one.name.clone(), false)),
            [] => start(&temzdf_client::auto_name, true),
            many => bail!(
                "several sessions, name one with -s: {}",
                many.iter().map(|s| s.name.as_str()).collect::<Vec<_>>().join(", ")
            ),
        },
    }
}
