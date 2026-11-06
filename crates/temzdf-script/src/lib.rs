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
pub use host::{UNATTENDED_LIMIT, run, run_link, run_with_limit};
