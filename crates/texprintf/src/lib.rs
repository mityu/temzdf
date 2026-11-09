//! TeX math as Unicode text, drawn by libtexprintf (the library behind
//! `utftex`): fractions, roots, sums and integrals over several lines of
//! box-drawing characters, sub- and superscripts as Unicode where they
//! exist.
//!
//! libtexprintf keeps its settings and error state in globals, so calls
//! go one at a time through a lock. Its settings are left as they are:
//! out-of-range character widths make it `exit(1)`.

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::sync::Mutex;

unsafe extern "C" {
    /// Draws `str` into a string libtexprintf allocates (free it with
    /// `texfree`); sets the error state read by `texerror_state`.
    fn texstring(str: *const c_char) -> *mut c_char;
    fn texfree(ptr: *mut c_void);
    fn texerror_state() -> c_int;
}

/// Longer formulas, or ones nested deeper, are not drawn: libtexprintf's
/// parser recurses (a thread's stack is 2 MiB) and deeply nested input
/// takes it very long (10000 nested `\frac` take over 20 s).
pub const MAX_LEN: usize = 4096;
pub const MAX_DEPTH: usize = 64;

/// One call at a time: libtexprintf's state is global.
static LOCK: Mutex<()> = Mutex::new(());

/// `tex` drawn as lines of text, or `None` when libtexprintf reports an
/// error or the formula is too long or deep ([`MAX_LEN`], [`MAX_DEPTH`]);
/// the caller shows the source instead.
pub fn render(tex: &str) -> Option<Vec<String>> {
    if tex.len() > MAX_LEN || depth(tex) > MAX_DEPTH {
        return None;
    }
    // A NUL would end the C string early: not TeX anyway.
    let input = CString::new(tex).ok()?;
    // A panic elsewhere while holding it leaves no state of ours behind.
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // SAFETY: `input` is a valid NUL-terminated string that outlives the
    // call; libtexprintf only reads it. The lock keeps any other call from
    // touching its globals meanwhile.
    let out = unsafe { texstring(input.as_ptr()) };
    if out.is_null() {
        return None;
    }
    // SAFETY: a non-null result of `texstring` is a NUL-terminated string
    // it allocated, valid until `texfree`; it is copied before that.
    let text = unsafe { CStr::from_ptr(out) }.to_string_lossy().into_owned();
    // SAFETY: still under the lock, right after the call that set it.
    let failed = unsafe { texerror_state() } != 0;
    // SAFETY: `out` came from `texstring` and is freed once, here, with
    // the matching deallocator; it is not used afterwards.
    unsafe { texfree(out.cast()) };
    if failed {
        return None;
    }
    let lines: Vec<String> = text.lines().map(|l| l.trim_end().to_owned()).collect();
    (!lines.is_empty()).then_some(lines)
}

/// How deeply groups nest in `tex`: braces, `\left` ... `\right` and
/// `\begin` ... `\end`.
fn depth(tex: &str) -> usize {
    let (mut d, mut max) = (0usize, 0);
    let mut rest = tex;
    while let Some(c) = rest.chars().next() {
        let opens = ["{", "\\left", "\\begin"].iter().find(|o| rest.starts_with(**o));
        let closes = ["}", "\\right", "\\end"].iter().find(|o| rest.starts_with(**o));
        if let Some(o) = opens {
            d += 1;
            max = max.max(d);
            rest = &rest[o.len()..];
        } else if let Some(o) = closes {
            d = d.saturating_sub(1);
            rest = &rest[o.len()..];
        } else if c == '\\' && rest.len() > 1 {
            // An escaped brace or backslash is no group.
            rest = &rest[1 + rest[1..].chars().next().map_or(0, char::len_utf8)..];
        } else {
            rest = &rest[c.len_utf8()..];
        }
    }
    max
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg_attr(miri, ignore = "calls into libtexprintf (C)")]
    fn fractions_take_three_lines() {
        assert_eq!(render(r"x = \frac{a}{b}").unwrap(), ["    a", "x = ─", "    b"]);
    }

    #[test]
    #[cfg_attr(miri, ignore = "calls into libtexprintf (C)")]
    fn scripts_and_symbols_are_unicode() {
        assert_eq!(render(r"\alpha^2 + \beta_1").unwrap(), ["α² + β₁"]);
    }

    #[test]
    #[cfg_attr(miri, ignore = "calls into libtexprintf (C)")]
    fn errors_and_nul_give_none() {
        assert_eq!(render(r"\frac{a"), None);
        assert_eq!(render("a\0b"), None);
        // The error state does not stick to the next call.
        assert!(render("x").is_some());
    }

    #[test]
    fn depth_counts_groups_not_escapes() {
        assert_eq!(depth(r"\frac{a}{b}"), 1);
        assert_eq!(depth(r"{{x}}"), 2);
        assert_eq!(depth(r"\left( \frac{a}{b} \right)"), 2);
        assert_eq!(depth(r"\{ \} \\ {x}"), 1);
    }

    #[test]
    #[cfg_attr(miri, ignore = "calls into libtexprintf (C)")]
    fn too_deep_or_long_is_not_drawn_and_the_limit_is_quick_in_a_thread() {
        let deep = |n: usize| format!("{}x{}", r"\frac{".repeat(n), "}{y}".repeat(n));
        assert_eq!(render(&deep(MAX_DEPTH + 1)), None);
        assert_eq!(render(&"x".repeat(MAX_LEN + 1)), None);
        // At the limit, in a worker thread's stack, quickly.
        let t = std::time::Instant::now();
        let out = std::thread::spawn(move || render(&deep(MAX_DEPTH))).join().unwrap();
        assert!(out.is_some());
        assert!(t.elapsed() < std::time::Duration::from_secs(2), "{:?}", t.elapsed());
    }

    #[test]
    #[cfg_attr(miri, ignore = "calls into libtexprintf (C)")]
    fn calls_from_threads_are_one_at_a_time() {
        let threads: Vec<_> = (0..8).map(|i| std::thread::spawn(move || render(&format!("x_{i}")))).collect();
        for (i, t) in threads.into_iter().enumerate() {
            assert_eq!(t.join().unwrap().unwrap(), [format!("x{}", char::from_u32(0x2080 + i as u32).unwrap())]);
        }
    }
}
