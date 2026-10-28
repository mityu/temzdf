//! The scale factor on X11 when the desktop sets no DPI.
//!
//! winit takes the scale from `WINIT_X11_SCALE_FACTOR`, else the DPI in
//! XSETTINGS (`Xft/DPI`) or the X resources (`Xft.dpi`), else the monitor's
//! physical size (pixels per millimetre). GTK, Qt and GLFW use scale 1
//! without a DPI setting, so on a dense monitor winit's last step made the
//! window and its text larger than every other application's (17/12 on a
//! 3840-pixel, 698 mm monitor). Without a DPI setting this sets
//! `WINIT_X11_SCALE_FACTOR=1`; with one winit reads it (and follows its
//! changes) as before.

use x11rb::connection::Connection;
use x11rb::protocol::xproto::{AtomEnum, ConnectionExt};

/// Set `WINIT_X11_SCALE_FACTOR=1` when the X server (`DISPLAY`) has no DPI
/// setting and the variable is unset.
///
/// # Safety
///
/// It sets an environment variable: no other thread may exist yet.
pub unsafe fn default_scale() {
    if std::env::var_os("WINIT_X11_SCALE_FACTOR").is_some() || std::env::var_os("DISPLAY").is_none() {
        return;
    }
    if has_dpi() == Some(false) {
        // SAFETY: the caller guarantees there is no other thread.
        unsafe { std::env::set_var("WINIT_X11_SCALE_FACTOR", "1") };
    }
}

/// Whether the X server has a DPI setting; `None` when it cannot be asked.
fn has_dpi() -> Option<bool> {
    let (conn, screen) = x11rb::connect(None).ok()?;
    let root = conn.setup().roots.get(screen)?.root;
    let atom = |name: &str| Some(conn.intern_atom(false, name.as_bytes()).ok()?.reply().ok()?.atom);
    let property = |window, name: u32, ty: u32| {
        Some(conn.get_property(false, window, name, ty, 0, u32::MAX).ok()?.reply().ok()?.value)
    };
    let settings = atom("_XSETTINGS_SETTINGS")?;
    let owner = conn.get_selection_owner(atom(&format!("_XSETTINGS_S{screen}"))?).ok()?.reply().ok()?.owner;
    if owner != x11rb::NONE && property(owner, settings, settings).is_some_and(|s| xsettings_have_dpi(&s)) {
        return Some(true);
    }
    let resources = property(root, AtomEnum::RESOURCE_MANAGER.into(), AtomEnum::STRING.into())?;
    Some(resources_have_dpi(&String::from_utf8_lossy(&resources)))
}

/// Whether an `_XSETTINGS_SETTINGS` property sets `Xft/DPI` (the XSETTINGS
/// specification: a byte order, a serial, then typed, named settings).
fn xsettings_have_dpi(data: &[u8]) -> bool {
    let big = matches!(data.first(), Some(1 | b'B'));
    let u16_at = |i: usize| {
        let b: [u8; 2] = data.get(i..i + 2)?.try_into().ok()?;
        Some(if big { u16::from_be_bytes(b) } else { u16::from_le_bytes(b) } as usize)
    };
    let u32_at = |i: usize| {
        let b: [u8; 4] = data.get(i..i + 4)?.try_into().ok()?;
        Some(if big { u32::from_be_bytes(b) } else { u32::from_le_bytes(b) } as usize)
    };
    let pad = |n: usize| n.div_ceil(4) * 4;
    let Some(count) = u32_at(8) else { return false };
    let mut at = 12;
    for _ in 0..count {
        let (Some(&kind), Some(name_len)) = (data.get(at), u16_at(at + 2)) else { return false };
        let Some(name) = data.get(at + 4..at + 4 + name_len) else { return false };
        if name == b"Xft/DPI" {
            return kind == 0;
        }
        // The name, then the serial of its last change, then the value.
        at += 4 + pad(name_len) + 4;
        at += match kind {
            0 => 4,
            1 => match u32_at(at) {
                Some(len) => 4 + pad(len),
                None => return false,
            },
            2 => 8,
            _ => return false,
        };
    }
    false
}

/// Whether X resources (`RESOURCE_MANAGER`) set `Xft.dpi`.
fn resources_have_dpi(resources: &str) -> bool {
    resources.lines().any(|l| l.strip_prefix("Xft.dpi").is_some_and(|rest| rest.trim_start().starts_with(':')))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An `_XSETTINGS_SETTINGS` property with `settings` (name, kind, value
    /// bytes), little-endian unless `big`.
    fn xsettings(big: bool, order: u8, settings: &[(&str, u8, Vec<u8>)]) -> Vec<u8> {
        let n32 = |v: u32| if big { v.to_be_bytes() } else { v.to_le_bytes() };
        let n16 = |v: u16| if big { v.to_be_bytes() } else { v.to_le_bytes() };
        let pad = |d: &mut Vec<u8>| d.resize(d.len().div_ceil(4) * 4, 0);
        let mut d = vec![order, 0, 0, 0];
        d.extend(n32(7));
        d.extend(n32(settings.len() as u32));
        for (name, kind, value) in settings {
            d.extend([*kind, 0]);
            d.extend(n16(name.len() as u16));
            d.extend(name.as_bytes());
            pad(&mut d);
            d.extend(n32(0));
            d.extend(value);
        }
        d
    }

    fn string(big: bool, s: &str) -> Vec<u8> {
        let mut v = (if big { (s.len() as u32).to_be_bytes() } else { (s.len() as u32).to_le_bytes() }).to_vec();
        v.extend(s.as_bytes());
        v.resize(4 + s.len().div_ceil(4) * 4, 0);
        v
    }

    #[test]
    fn xsettings_dpi_is_found_past_other_settings_in_either_byte_order() {
        for (big, order) in [(false, 0), (false, b'l'), (true, 1), (true, b'B')] {
            let int = |v: u32| (if big { v.to_be_bytes() } else { v.to_le_bytes() }).to_vec();
            let others = vec![
                ("Gtk/IconSizes", 1, string(big, "")),
                ("Xft/RGBA", 1, string(big, "none")),
                ("Gtk/CursorThemeColor", 2, vec![0; 8]),
                ("Gdk/WindowScalingFactor", 0, int(1)),
            ];
            assert!(!xsettings_have_dpi(&xsettings(big, order, &others)), "XFCE's, without a DPI (order {order})");
            let mut with = others.clone();
            with.push(("Xft/DPI", 0, int(120 * 1024)));
            assert!(xsettings_have_dpi(&xsettings(big, order, &with)), "order {order}");
        }
        assert!(!xsettings_have_dpi(&[]));
        let cut = xsettings(false, 0, &[("Xft/RGBA", 1, string(false, "none")), ("Xft/DPI", 0, vec![0; 4])]);
        assert!(!xsettings_have_dpi(&cut[..20]), "a truncated property");
    }

    #[test]
    fn resources_dpi_is_a_line_of_its_own() {
        assert!(resources_have_dpi("Xft.antialias:\t1\nXft.dpi:\t144\n"));
        assert!(resources_have_dpi("Xft.dpi : 96"));
        assert!(!resources_have_dpi("Xft.autohint:\t0\nXft.lcdfilter:\tlcddefault\nXcursor.size:\t0\n"));
        assert!(!resources_have_dpi("Xft.dpiScale:\t2\n! Xft.dpi: 144\n"));
    }
}
