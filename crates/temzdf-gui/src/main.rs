//! `temzdf-gui`: show a temzdf session in a window (doc/design/gui-client.md).

// No console window of its own on Windows; output goes to the console of
// whoever started it (`attach_console`).
#![cfg_attr(windows, windows_subsystem = "windows")]

use anyhow::{Context, Result};
use clap::Parser;
use temzdf_gui::cli::{Cli, Command, font_entry};
use temzdf_gui::connection::session_for;

fn main() -> Result<()> {
    #[cfg(windows)]
    attach_console();
    // SAFETY: the first thing the program does, before any other thread
    // exists.
    #[cfg(all(unix, not(target_os = "macos")))]
    unsafe {
        temzdf_gui::x11scale::default_scale()
    };
    let cli = Cli::parse();
    if let Some(Command::LsFonts(a)) = &cli.command {
        let font: Vec<_> = a.font.iter().map(|f| font_entry(f)).collect();
        let wide: Vec<_> = a.font_wide.iter().map(|f| font_entry(f)).collect();
        let mut fonts = temzdf_gui::fonts::system();
        print!("{}", temzdf_gui::lsfonts::report(&mut fonts, &font, &wide, a.text.as_deref(), a.list_system));
        return Ok(());
    }
    let (name, started) = session_for(cli.session.as_deref(), cli.file.as_deref(), cli.clean)?;
    if let Some(out) = &cli.render_frame {
        let (w, h) = cli.size.split_once('x').context("--size is WxH")?;
        let size = (w.parse().context("--size width")?, h.parse().context("--size height")?);
        let result = temzdf_gui::headless::render_frame(&name, out, size, cli.scale);
        // A session started only to be drawn goes again.
        if started {
            let _ = temzdf_client::request(&name, temzdf_protocol::ClientMsg::Kill, temzdf_client::REQUEST_TIMEOUT);
        }
        return result;
    }
    let line = temzdf_gui::app::run(name)?;
    if !line.is_empty() {
        println!("{line}");
    }
    Ok(())
}

/// Output where a console program's would go: a GUI program on Windows
/// starts without the standard handles of the console it was started from
/// (redirected ones it does get, and keeps). The missing ones are opened on
/// that console (`ls-fonts`, `--render-frame`'s errors, clap's help and
/// the line after the window closes), if there is one: none when started
/// from Explorer or the Start menu. The console does not wait for a GUI
/// program, so in an interactive shell the output can come after the next
/// prompt.
#[cfg(windows)]
fn attach_console() {
    use std::os::windows::io::IntoRawHandle;
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::System::Console::{
        ATTACH_PARENT_PROCESS, AttachConsole, GetStdHandle, STD_ERROR_HANDLE, STD_HANDLE, STD_OUTPUT_HANDLE,
        SetStdHandle,
    };
    // SAFETY: reads this process's standard handle table.
    let missing = |which: STD_HANDLE| unsafe {
        let h = GetStdHandle(which);
        h.is_null() || h == INVALID_HANDLE_VALUE
    };
    let missing: Vec<_> = [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE].into_iter().filter(|&w| missing(w)).collect();
    // SAFETY: a plain Win32 call; it fails without a parent console.
    if missing.is_empty() || unsafe { AttachConsole(ATTACH_PARENT_PROCESS) } == 0 {
        return;
    }
    for which in missing {
        if let Ok(f) = std::fs::OpenOptions::new().read(true).write(true).open("CONOUT$") {
            // SAFETY: a handle of ours, given to the process for good (the
            // standard handles are never closed).
            unsafe { SetStdHandle(which, f.into_raw_handle()) };
        }
    }
}
