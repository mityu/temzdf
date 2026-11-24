//! UTF-8 for the prelude's `TextEncoder` and `TextDecoder`
//! (src/text_codec.ts): the byte work, natively (QuickJS has neither, and a
//! loop over bytes in JavaScript is slow on command output of megabytes).
//! The classes' state (a stream's held bytes, the BOM) is the prelude's.

/// `bytes` as text: with `fatal`, `None` at the first invalid sequence;
/// otherwise each maximal invalid subpart is U+FFFD, as the WHATWG
/// Encoding Standard says (which `from_utf8_lossy` does). With `stream`,
/// an incomplete sequence at the end is not decoded: the second value is
/// how many bytes it is, for the next call.
pub fn decode(bytes: &[u8], fatal: bool, stream: bool) -> Option<(String, usize)> {
    let keep = if stream { incomplete_tail(bytes) } else { 0 };
    let bytes = &bytes[..bytes.len() - keep];
    let text =
        if fatal { std::str::from_utf8(bytes).ok()?.to_owned() } else { String::from_utf8_lossy(bytes).into_owned() };
    Some((text, keep))
}

/// How many bytes at the end of `bytes` start a sequence the next bytes
/// may complete (0 to 3).
fn incomplete_tail(bytes: &[u8]) -> usize {
    (1..=3.min(bytes.len()))
        .find(|&k| {
            matches!(std::str::from_utf8(&bytes[bytes.len() - k..]),
                Err(e) if e.valid_up_to() == 0 && e.error_len().is_none())
        })
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_sequences_become_replacement_characters() {
        assert_eq!(decode("aあ😀".as_bytes(), false, false), Some(("aあ😀".into(), 0)));
        // A lone continuation byte, a truncated sequence, an overlong one.
        assert_eq!(decode(b"a\x80b", false, false), Some(("a\u{fffd}b".into(), 0)));
        assert_eq!(decode(b"\xe3\x81", false, false), Some(("\u{fffd}".into(), 0)));
        assert_eq!(decode(b"\xc0\xaf", false, false), Some(("\u{fffd}\u{fffd}".into(), 0)));
        assert_eq!(decode(b"a\x80", true, false), None, "fatal");
    }

    #[test]
    fn a_stream_keeps_an_incomplete_sequence_at_its_end() {
        let smile = "😀".as_bytes();
        for cut in 1..smile.len() {
            let (text, keep) = decode(&smile[..cut], false, true).unwrap();
            assert_eq!((text.as_str(), keep), ("", cut), "{cut} bytes of four");
        }
        assert_eq!(decode(b"a\xe3\x81", true, true), Some(("a".into(), 2)));
        // Invalid, not incomplete: decoded (replaced) now.
        assert_eq!(decode(b"a\x80", false, true), Some(("a\u{fffd}".into(), 0)));
        assert_eq!(decode(b"\xe3\xe3\x81", false, true), Some(("\u{fffd}".into(), 2)));
    }
}
