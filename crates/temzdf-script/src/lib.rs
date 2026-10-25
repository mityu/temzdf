//! The script host: runs the user's JavaScript/TypeScript configuration in
//! QuickJS (rquickjs) and talks to the session over
//! `temzdf_protocol::script`. temzdf starts it as `temzdf script-host`. See
//! doc/script-host.md.

mod host;
pub mod strip;
pub mod types;

pub use host::run;
