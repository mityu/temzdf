//! The script host: runs the user's JavaScript/TypeScript configuration in
//! QuickJS (rquickjs) and talks to the session over
//! `temzdf_protocol::script`. temzdf starts it as `temzdf script-host`. See
//! doc/design/script-host.md.

#[cfg(feature = "host")]
mod host;
#[cfg(feature = "host")]
pub mod link;
#[cfg(feature = "host")]
pub mod strip;
pub mod types;

#[cfg(feature = "host")]
pub use host::{run, run_link, run_with_limit};

/// How long a timer or a promise job (code nobody waits for, so nobody can
/// interrupt) may run before it is interrupted.
pub const UNATTENDED_LIMIT: std::time::Duration = std::time::Duration::from_secs(10);
