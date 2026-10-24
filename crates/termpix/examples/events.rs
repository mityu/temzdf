//! Log every input event termpix parses, one per line, to the file given as
//! the first argument (stdout is the UI). Press q (or Ctrl+C) to quit.
//! Useful to see what a terminal actually sends: `cargo run -p termpix --example events -- events.log`.

use std::io::Write;
use std::sync::mpsc;
use std::time::Duration;

use termpix::input::{Event, KeyCode, KeyEventKind, Modifiers};
use termpix::tty::{self, Modes, TermEvent, Tty};

fn main() -> std::io::Result<()> {
    let path = std::env::args().nth(1).unwrap_or_else(|| "events.log".into());
    let mut log = std::fs::File::create(&path)?;
    let mut tty = Tty::open(Modes::default())?;
    let caps = termpix::probe::probe(&mut tty, Duration::from_secs(1))?;
    if caps.pixel_mouse {
        tty.enable_pixel_mouse()?;
    }
    writeln!(log, "# {caps:?}")?;
    let (tx, rx) = mpsc::channel();
    tty::spawn_input(tty.reader()?, caps.pixel_mouse, tx, |e| e)?;
    for ev in rx {
        writeln!(log, "{ev:?}")?;
        log.flush()?;
        if let TermEvent::Input(Event::Key(k)) = &ev
            && k.kind != KeyEventKind::Release
            && (k.code == KeyCode::Char('q') || (k.code == KeyCode::Char('c') && k.modifiers.contains(Modifiers::CTRL)))
        {
            break;
        }
    }
    Ok(())
}
