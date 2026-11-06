// The engine's configuration, shared by build.rs (precompiling) and the
// runtime (loading): a precompiled module only loads into an engine
// configured the same way.

/// Configure an engine for the script host's guest.
pub fn configure(config: &mut wasmtime::Config) {
    // ScriptHost::stop: the only epoch change, so the deadline is otherwise
    // never reached.
    config.epoch_interruption(true);
}
