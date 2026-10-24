//! Print what `termpix::probe` detects in the current terminal, and the raw
//! environment hints. Useful when a terminal behaves unexpectedly:
//! `cargo run -p termpix --example probe`.

use std::time::Duration;

use termpix::tty::{Modes, Tty};

fn main() -> std::io::Result<()> {
    let caps = {
        let modes = Modes { alternate_screen: false, mouse: false, keyboard_flags: 0, ..Modes::default() };
        let mut tty = Tty::open(modes)?;
        termpix::probe::probe(&mut tty, Duration::from_secs(2))?
    };
    println!("{caps:#?}");
    println!("backend: {:?}", caps.backend());
    println!("medium:  {:?}", caps.medium());
    println!("env:     {:#?}", termpix::probe::Env::from_process());
    Ok(())
}
