//! The script host's cases (tests/host/cases.rs), run natively: the host on a
//! thread of its own over Unix sockets.

use std::os::unix::net::UnixStream;
use std::time::Duration;

#[path = "host/cases.rs"]
mod cases;

fn spawn_host(host_in: UnixStream, host_out: UnixStream, limit: Duration) {
    std::thread::spawn(move || temzdf_script::run_with_limit(host_in, host_out, limit).unwrap());
}
