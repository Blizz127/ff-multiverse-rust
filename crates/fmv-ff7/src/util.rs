//! Panic-free little-endian readers over untrusted byte slices.
//!
//! Every function returns `None` instead of panicking when the requested range does not
//! fit inside `data` (including the case where `off + width` would overflow `usize`).

/// Borrow `len` bytes starting at `off`, or `None` if they do not all exist.
pub(crate) fn slice_at(data: &[u8], off: usize, len: usize) -> Option<&[u8]> {
    let end = off.checked_add(len)?;
    data.get(off..end)
}

pub(crate) fn u8_at(data: &[u8], off: usize) -> Option<u8> {
    data.get(off).copied()
}

pub(crate) fn u16_at(data: &[u8], off: usize) -> Option<u16> {
    let bytes: [u8; 2] = slice_at(data, off, 2)?.try_into().ok()?;
    Some(u16::from_le_bytes(bytes))
}

pub(crate) fn u32_at(data: &[u8], off: usize) -> Option<u32> {
    let bytes: [u8; 4] = slice_at(data, off, 4)?.try_into().ok()?;
    Some(u32::from_le_bytes(bytes))
}

/// Decode a fixed-width, NUL-padded ASCII name field. Bytes >= 0x80 are mapped through
/// Latin-1 so the result is always valid UTF-8 and the mapping never fails.
pub(crate) fn nul_padded_name(raw: &[u8]) -> String {
    raw.iter().take_while(|&&b| b != 0).map(|&b| char::from(b)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readers_are_bounds_checked() {
        let d = [1u8, 2, 3, 4, 5];
        assert_eq!(u8_at(&d, 4), Some(5));
        assert_eq!(u8_at(&d, 5), None);
        assert_eq!(u16_at(&d, 0), Some(0x0201));
        assert_eq!(u16_at(&d, 4), None);
        assert_eq!(u32_at(&d, 1), Some(0x0504_0302));
        assert_eq!(u32_at(&d, 2), None);
        // offset arithmetic must not overflow-panic
        assert_eq!(u32_at(&d, usize::MAX), None);
        assert_eq!(slice_at(&d, usize::MAX - 1, 8), None);
        assert_eq!(slice_at(&d, 5, 0), Some(&[][..]));
    }

    #[test]
    fn names_stop_at_nul_and_never_fail() {
        assert_eq!(nul_padded_name(b"md1stin\0\0\0\0"), "md1stin");
        assert_eq!(nul_padded_name(b"abc"), "abc");
        assert_eq!(nul_padded_name(&[0x41, 0xE9, 0x00, 0x42]), "A\u{e9}");
        assert_eq!(nul_padded_name(&[]), "");
    }
}
