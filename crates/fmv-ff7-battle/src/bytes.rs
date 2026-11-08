//! Total (never-panicking) little-endian readers and a capped gzip helper for untrusted data.
//!
//! Every parser in this crate validates record lengths first and then reads fields through
//! these helpers. Out-of-range reads yield zero instead of panicking, so a logic slip can
//! never turn a hostile file into a crash.

use std::io::Read;

use flate2::read::GzDecoder;

/// Copy `N` bytes starting at `o`, or all zeros if the range is out of bounds.
fn arr<const N: usize>(b: &[u8], o: usize) -> [u8; N] {
    let mut out = [0u8; N];
    if let Some(src) = o.checked_add(N).and_then(|end| b.get(o..end)) {
        out.copy_from_slice(src);
    }
    out
}

pub(crate) fn u8_at(b: &[u8], o: usize) -> u8 {
    b.get(o).copied().unwrap_or(0)
}

pub(crate) fn i8_at(b: &[u8], o: usize) -> i8 {
    u8_at(b, o) as i8
}

pub(crate) fn u16_at(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes(arr(b, o))
}

pub(crate) fn i16_at(b: &[u8], o: usize) -> i16 {
    i16::from_le_bytes(arr(b, o))
}

pub(crate) fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(arr(b, o))
}

/// Fixed-size sub-array at `o` (zeros when out of bounds).
pub(crate) fn array_at<const N: usize>(b: &[u8], o: usize) -> [u8; N] {
    arr(b, o)
}

/// `N` consecutive little-endian u16 values starting at `o`.
pub(crate) fn u16_array_at<const N: usize>(b: &[u8], o: usize) -> [u16; N] {
    let mut out = [0u16; N];
    for (i, slot) in out.iter_mut().enumerate() {
        *slot = u16_at(b, o.saturating_add(i.saturating_mul(2)));
    }
    out
}

/// Decompress the first gzip member of `data`, producing at most `cap + 1` bytes.
///
/// The caller treats a result longer than `cap` as "too large". Reading through to the end of
/// the member (when it fits) also verifies the gzip CRC and length trailer.
pub(crate) fn gunzip_capped(data: &[u8], cap: usize) -> std::io::Result<Vec<u8>> {
    let mut out = Vec::new();
    GzDecoder::new(data)
        .take((cap as u64).saturating_add(1))
        .read_to_end(&mut out)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_are_total() {
        let b = [1u8, 2, 3, 4];
        assert_eq!(u16_at(&b, 0), 0x0201);
        assert_eq!(u32_at(&b, 0), 0x0403_0201);
        assert_eq!(i16_at(&[0xFF, 0xFF], 0), -1);
        // Out of range, partially out of range and overflowing offsets never panic.
        assert_eq!(u16_at(&b, 3), 0);
        assert_eq!(u32_at(&b, 1), 0);
        assert_eq!(u32_at(&b, usize::MAX), 0);
        assert_eq!(u8_at(&b, 4), 0);
        assert_eq!(u16_array_at::<3>(&b, 2), [0x0403, 0, 0]);
    }

    #[test]
    fn gunzip_cap_is_enforced() {
        use flate2::{write::GzEncoder, Compression};
        use std::io::Write;
        let mut enc = GzEncoder::new(Vec::new(), Compression::fast());
        enc.write_all(&[7u8; 1000]).unwrap();
        let gz = enc.finish().unwrap();
        assert_eq!(gunzip_capped(&gz, 1000).unwrap().len(), 1000);
        // A tighter cap yields cap + 1 bytes, which callers read as "too large".
        assert_eq!(gunzip_capped(&gz, 10).unwrap().len(), 11);
        assert!(gunzip_capped(&[1, 2, 3], 10).is_err());
    }
}
