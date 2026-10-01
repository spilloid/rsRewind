/// UTF-16 buffer → `String`, stopping at the first NUL. Lossy: unpaired surrogates (which window
/// titles can legally contain) become U+FFFD instead of failing the whole read.
pub(crate) fn from_wide(buffer: &[u16]) -> String {
    let len = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..len])
}

/// File name component of a Windows path (`C:\x\Teams.exe` → `Teams.exe`).
pub(crate) fn file_name(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stops_at_nul() {
        let buf: Vec<u16> = "abc\0def".encode_utf16().collect();
        assert_eq!(from_wide(&buf), "abc");
        let full: Vec<u16> = "xyz".encode_utf16().collect();
        assert_eq!(from_wide(&full), "xyz");
    }

    #[test]
    fn unpaired_surrogate_is_replaced() {
        assert_eq!(from_wide(&[0x61, 0xD800, 0x62]), "a\u{FFFD}b");
    }

    #[test]
    fn file_names() {
        assert_eq!(file_name(r"C:\Program Files\App\Teams.exe"), "Teams.exe");
        assert_eq!(file_name("Teams.exe"), "Teams.exe");
        assert_eq!(file_name(r"\\?\C:\a/b.exe"), "b.exe");
    }
}
