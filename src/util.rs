//! Tiny helpers shared across modules. Keep this file small.

/// Encode a `&str` as a NUL-terminated UTF-16 buffer, ready to hand to a
/// Win32 `PCWSTR` argument. The caller is responsible for keeping the
/// returned `Vec<u16>` alive for the duration of the FFI call.
#[must_use]
pub fn wide_nul(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_nul_is_terminated() {
        let w = wide_nul("hi");
        assert_eq!(w, vec![u16::from(b'h'), u16::from(b'i'), 0]);
    }

    #[test]
    fn wide_nul_empty_string() {
        assert_eq!(wide_nul(""), vec![0]);
    }

    #[test]
    fn wide_nul_unicode() {
        let w = wide_nul("é");
        assert_eq!(w.last(), Some(&0));
        // 'é' is U+00E9, fits in a single UTF-16 code unit.
        assert_eq!(w.len(), 2);
        assert_eq!(w[0], 0x00E9);
    }
}
