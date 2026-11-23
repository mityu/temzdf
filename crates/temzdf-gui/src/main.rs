//! `temzdf-gui`: show a temzdf session in a window (doc/design/gui-client.md).

// No console window on Windows, and no output to the console it was started
// from (the shell would not wait for it): output goes where it is
// redirected, errors without anywhere to go into a message box
// (doc/design/gui-client.md).
#![cfg_attr(windows, windows_subsystem = "windows")]

use anyhow::{Context, Result};
use clap::Parser;
use temzdf_gui::cli::{Cli, Command, font_entry};
use temzdf_gui::connection::session_for;

fn main() -> std::process::ExitCode {
    // SAFETY: the first thing the program does, before any other thread
    // exists.
    #[cfg(all(unix, not(target_os = "macos")))]
    unsafe {
        temzdf_gui::x11scale::default_scale()
    };
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        #[cfg(windows)]
        Err(e) if !stderr_present() => {
            let info = matches!(e.kind(), clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion);
            message_box(&e.render().to_string(), !info);
            return u8::try_from(e.exit_code()).unwrap_or(2).into();
        }
        Err(e) => e.exit(),
    };
    match run(&cli) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            #[cfg(windows)]
            if !stderr_present() {
                message_box(&format!("{e:#}"), true);
                return std::process::ExitCode::FAILURE;
            }
            eprintln!("Error: {e:?}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run(cli: &Cli) -> Result<()> {
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

/// Whether this process has a standard error to write to: a GUI program on
/// Windows has none unless it was redirected (or inherited from a program
/// that has one, as over SSH). std documents a null handle for a missing
/// one; INVALID_HANDLE_VALUE is checked too, as std does not document
/// mapping it.
#[cfg(windows)]
fn stderr_present() -> bool {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    let h = std::io::stderr().as_raw_handle();
    !h.is_null() && h != INVALID_HANDLE_VALUE
}

/// Show `text` in a message box: an error's, or information (help).
#[cfg(windows)]
fn message_box(text: &str, error: bool) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_ICONINFORMATION, MB_OK, MessageBoxW};
    let wide = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
    let (text, title) = (wide(text.trim_end()), wide("temzdf-gui"));
    let icon = if error { MB_ICONERROR } else { MB_ICONINFORMATION };
    // SAFETY: both strings are NUL-terminated UTF-16 and outlive the call;
    // no owner window.
    unsafe { MessageBoxW(std::ptr::null_mut(), text.as_ptr(), title.as_ptr(), MB_OK | icon) };
}
