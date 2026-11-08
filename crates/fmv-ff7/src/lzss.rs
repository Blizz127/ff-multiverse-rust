//! FF7 LZSS decompressor.
//!
//! **Verified on owned FF7 Steam Edition (en) on 2026-11-08.** Decoding all 702 field entries of
//! the owner's `flevel.lgp` (279 241 487 bytes in total, the largest field 1 296 532 bytes)
//! reproduces the file layout exactly: the nine section lengths plus the 78 header bytes equal
//! the decompressed size in 120 fields and fall short by exactly the 14-byte `FINAL FANTASY7`
//! trailer in the other 582, with every section then parsing. The size header equals the stored
//! entry length minus 4 in all 702 entries (none uses the "header counts itself" variant, which
//! stays accepted). `md1stin`: 201 335 stored bytes, header 201 331, 414 915 bytes expanded.
//! No decompressed size is stored anywhere, so that section-table equality is the check.
//!
//! # Sources
//!
//! * FFRTT wiki, "FF7/LZSS format" <https://wiki.ffrtt.ru/index.php/FF7/LZSS_format>
//!   (fetched through a summarising tool, not copied verbatim): size header, LSB-first
//!   control bits with `0 = reference` / `1 = literal`, reference layout `OOOOOOOO OOOOLLLL`,
//!   4096-byte zero-initialised ring buffer whose write position starts at `0xFEE`, and the
//!   output-relative conversion `real = tail - ((tail - 18 - raw_offset) mod 4096)`.
//! * PyFF7 wiki, "LZSS Format" <https://github.com/niemasd/PyFF7/wiki/LZSS-Format>
//!   (also fetched through a summarising tool): the 4-byte header is "the size of the
//!   compressed data (should be LZSS filesize - 4)", control bits LSB-first, the same
//!   reference layout and `0xFEE` ring start, references before the start of the output
//!   read as zeros, overlapping copies repeat, and the (hedged) note about a final
//!   "reference that is only 1 byte" in some non-field files, which motivates the
//!   lone-final-byte rule below.
//! * Cross-check only (search-result summary, not fetched in full): `cebix/ff7tools`
//!   `ff7/lzss.py` uses the same conversion with `0xFEE`.
//!
//! # Stream layout
//!
//! ```text
//! u32 LE   compressed size (see "Header" below)
//! repeat:  1 control byte, then up to 8 items, one per control bit starting at the LSB
//!            bit = 1  literal   : 1 byte copied to the output
//!            bit = 0  reference : 2 bytes  b0 = offset bits 0..=7
//!                                          b1 = (offset bits 8..=11) << 4 | (length - 3)
//!                                 offset is an ABSOLUTE index into the 4096-byte ring,
//!                                 length is 3..=18
//! ```
//!
//! The ring starts zero-filled with its write cursor at `0xFEE`; every output byte (literal
//! or copied) is also written to the ring. References are copied one byte at a time, which
//! makes overlapping copies (length greater than distance) repeat the pattern, and makes a
//! reference into a never-written part of the ring yield `0x00`. Working in ring space also
//! means the "convert to output-relative offset" step of the wiki is implicit and exact.
//!
//! # Header
//!
//! The wiki calls the header "the length of the compressed file" without saying whether the
//! four header bytes are counted. PyFF7 states it equals the file size minus four. This
//! decoder accepts both: the header value may equal the bytes after the header (normal), or
//! the whole buffer length (header counts itself), and it may be smaller than the bytes
//! available (trailing padding is ignored). A larger value is an error.
//!
//! # End of stream
//!
//! There is no stored uncompressed size. The stream ends when the input is exhausted at an
//! item boundary (the unused trailing control bits of the last group are ignored). A single
//! leftover byte where a reference's second byte should be can only be the final byte of
//! the input and is also treated as the end of the stream rather than an error: PyFF7's notes
//! report real FF7 files with an oddly terminated final item ("a reference that is only 1
//! byte"), so rejecting that case could reject valid media. Truncation that matters is
//! caught earlier, by the size header (a header larger than the data is an error).
//!
//! # Safety against untrusted input
//!
//! No indexing on input data, no panics, output is capped (default
//! [`DEFAULT_MAX_OUTPUT`]) so a hostile stream cannot allocate without bound.

use crate::util::u32_at;
use thiserror::Error;

/// Bytes of the leading size header.
pub const HEADER_LEN: usize = 4;
/// Ring-buffer size (12-bit offsets).
pub const RING_SIZE: usize = 4096;
/// Initial ring write position.
pub const RING_START: usize = 0xFEE;
/// Shortest encodable reference.
pub const MIN_MATCH: usize = 3;
/// Longest encodable reference.
pub const MAX_MATCH: usize = 18;
/// Default cap on decompressed size (64 MiB, far above any real field file).
pub const DEFAULT_MAX_OUTPUT: usize = 64 * 1024 * 1024;

const RING_MASK: usize = RING_SIZE - 1;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum LzssError {
    #[error("compressed data is {len} bytes, shorter than the {HEADER_LEN}-byte size header")]
    MissingHeader { len: usize },
    #[error("size header declares {declared} compressed bytes but only {available} follow it")]
    Truncated { declared: u64, available: usize },
    #[error("decompressed size would exceed the limit of {limit} bytes")]
    OutputTooLarge { limit: usize },
}

/// Decompress a size-prefixed FF7 LZSS buffer (as stored for a field file inside
/// `flevel.lgp`), capped at [`DEFAULT_MAX_OUTPUT`].
pub fn decompress(data: &[u8]) -> Result<Vec<u8>, LzssError> {
    decompress_with_limit(data, DEFAULT_MAX_OUTPUT)
}

/// Like [`decompress`] with an explicit output cap.
pub fn decompress_with_limit(data: &[u8], max_output: usize) -> Result<Vec<u8>, LzssError> {
    let declared = u32_at(data, 0).ok_or(LzssError::MissingHeader { len: data.len() })?;
    let declared = u64::from(declared);
    let body = data.get(HEADER_LEN..).unwrap_or(&[]);
    let available = body.len();
    let payload = if declared <= available as u64 {
        // `declared <= available`, so this range exists; the fallback is unreachable.
        usize::try_from(declared).ok().and_then(|n| body.get(..n))
    } else if declared == data.len() as u64 {
        // Header counts itself.
        Some(body)
    } else {
        None
    };
    let payload = payload.ok_or(LzssError::Truncated { declared, available })?;
    decompress_raw(payload, max_output)
}

/// Decompress a bare LZSS stream (no 4-byte size header).
pub fn decompress_raw(payload: &[u8], max_output: usize) -> Result<Vec<u8>, LzssError> {
    let mut ring = Ring::new();
    // Typical ratio is 2-3x; never reserve beyond the cap.
    let mut out: Vec<u8> = Vec::with_capacity(payload.len().saturating_mul(3).min(max_output));
    let mut input = payload.iter().copied();

    while let Some(control) = input.next() {
        for bit in 0..8u32 {
            // Input exhausted exactly at an item boundary: normal end of stream.
            let Some(first) = input.next() else {
                return Ok(out);
            };
            if (control >> bit) & 1 == 1 {
                if out.len() >= max_output {
                    return Err(LzssError::OutputTooLarge { limit: max_output });
                }
                out.push(first);
                ring.push(first);
            } else {
                let Some(second) = input.next() else {
                    // Lone final byte: end of stream (see the module docs).
                    return Ok(out);
                };
                // Widen BEFORE shifting: the high nibble moves into bits 8..=11.
                let offset = usize::from(first) | ((usize::from(second) & 0xF0) << 4);
                let len = usize::from(second & 0x0F) + MIN_MATCH;
                // `out.len() <= max_output` always holds here, so this cannot overflow.
                if out.len() + len > max_output {
                    return Err(LzssError::OutputTooLarge { limit: max_output });
                }
                for k in 0..len {
                    let byte = ring.at(offset + k);
                    out.push(byte);
                    ring.push(byte);
                }
            }
        }
    }
    Ok(out)
}

/// 4096-byte ring with a write cursor. All indices are masked, so access is always in
/// range; `get`/`get_mut` keep the module free of indexing.
struct Ring {
    buf: [u8; RING_SIZE],
    pos: usize,
}

impl Ring {
    fn new() -> Self {
        Self { buf: [0u8; RING_SIZE], pos: RING_START }
    }

    fn push(&mut self, byte: u8) {
        if let Some(slot) = self.buf.get_mut(self.pos & RING_MASK) {
            *slot = byte;
        }
        self.pos = (self.pos + 1) & RING_MASK;
    }

    fn at(&self, index: usize) -> u8 {
        self.buf.get(index & RING_MASK).copied().unwrap_or(0)
    }
}

/// Test-only greedy compressor, used for round-trip tests and to build synthetic
/// `flevel.lgp` entries. It is deliberately simple and NOT part of the public API.
#[cfg(test)]
pub(crate) fn compress(data: &[u8]) -> Vec<u8> {
    let mut body: Vec<u8> = Vec::new();
    let mut pos = 0usize;
    while pos < data.len() {
        let ctrl_at = body.len();
        body.push(0);
        let mut ctrl = 0u8;
        for bit in 0..8 {
            if pos >= data.len() {
                break;
            }
            let max_dist = pos.min(RING_SIZE - 1);
            let mut best_len = 0usize;
            let mut best_dist = 0usize;
            for dist in 1..=max_dist {
                let mut l = 0usize;
                while l < MAX_MATCH && pos + l < data.len() && data[pos + l] == data[pos - dist + l] {
                    l += 1;
                }
                if l > best_len {
                    best_len = l;
                    best_dist = dist;
                    if l == MAX_MATCH {
                        break;
                    }
                }
            }
            if best_len >= MIN_MATCH {
                let offset = (RING_START + (pos - best_dist)) & RING_MASK;
                body.push((offset & 0xFF) as u8);
                body.push((((offset >> 4) & 0xF0) | (best_len - MIN_MATCH)) as u8);
                pos += best_len;
            } else {
                ctrl |= 1 << bit;
                body.push(data[pos]);
                pos += 1;
            }
        }
        body[ctrl_at] = ctrl;
    }
    let mut out = (body.len() as u32).to_le_bytes().to_vec();
    out.extend_from_slice(&body);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Prepend the size header for a hand-built body.
    fn with_header(body: &[u8]) -> Vec<u8> {
        let mut v = (body.len() as u32).to_le_bytes().to_vec();
        v.extend_from_slice(body);
        v
    }

    /// Tiny deterministic PRNG so tests need no extra crates.
    struct XorShift(u64);
    impl XorShift {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
    }

    // ---- hand-built vectors derived from the documented rules (independent of compress) ----

    #[test]
    fn reference_to_ring_index_0xfee_repeats_first_output_bytes() {
        // control 0b0000_0111: three literals, then a reference (bit 3 = 0).
        // 'A' was written to ring[0xFEE], so offset = 0xFEE: b0 = 0xEE, high nibble 0xF.
        // length 6 -> low nibble 3. The copy reads bytes it has just written (overlap).
        let body = [0x07, b'A', b'B', b'C', 0xEE, 0xF3];
        assert_eq!(decompress(&with_header(&body)).unwrap(), b"ABCABCABC");
    }

    #[test]
    fn reference_into_never_written_ring_yields_zeros() {
        // First item is a reference to ring index 0 (never written): three 0x00 bytes.
        let body = [0x00, 0x00, 0x00];
        assert_eq!(decompress(&with_header(&body)).unwrap(), vec![0u8; 3]);
        // Index 0xFED is the byte just BEFORE the first write position: also zero.
        let body = [0x00, 0xED, 0xF0];
        assert_eq!(decompress(&with_header(&body)).unwrap(), vec![0u8; 3]);
    }

    #[test]
    fn overlapping_copy_longer_than_distance_repeats_pattern() {
        // 'A','B' then a reference at offset 0xFEE (distance 2) of length 6: ABABABAB.
        let body = [0x03, b'A', b'B', 0xEE, 0xF3];
        assert_eq!(decompress(&with_header(&body)).unwrap(), b"ABABABAB");
    }

    #[test]
    fn wiki_worked_conversion_tail_1000_offset_0x153_length_5_copies_from_357() {
        // FFRTT: real = tail - ((tail - 18 - raw) mod 4096); tail=1000, raw=0x153, len=5 -> 357.
        let literals: Vec<u8> = (0..1000u32).map(|i| (i.wrapping_mul(7).wrapping_add(3) & 0xFF) as u8).collect();
        let mut body = Vec::new();
        for chunk in literals.chunks(8) {
            body.push(0xFF);
            body.extend_from_slice(chunk);
        }
        // One reference item: control bit0 = 0. Bytes `53 12` are the wiki's example:
        // low 8 offset bits 0x53, high offset nibble 0x1, length-3 = 2.
        body.extend_from_slice(&[0x00, 0x53, 0x12]);
        let out = decompress(&with_header(&body)).unwrap();
        assert_eq!(out.len(), 1005);
        assert_eq!(out.get(..1000), Some(&literals[..]));
        assert_eq!(out.get(1000..1005), literals.get(357..362));
    }

    #[test]
    fn control_bits_are_lsb_first_with_one_meaning_literal() {
        // 0b0000_0010: item0 = reference, item1 = literal. Reference to ring[0] (zeros, len 3),
        // then literal 'Z'. The following items have no data, so the stream ends.
        let body = [0x02, 0x00, 0x00, b'Z'];
        assert_eq!(decompress(&with_header(&body)).unwrap(), [0, 0, 0, b'Z']);
    }

    #[test]
    fn header_variants_and_padding() {
        let body = [0x07, b'A', b'B', b'C', 0xEE, 0xF3];
        // Header larger than what follows but equal to the whole buffer: header counts itself.
        let mut inclusive = ((body.len() + HEADER_LEN) as u32).to_le_bytes().to_vec();
        inclusive.extend_from_slice(&body);
        assert_eq!(decompress(&inclusive).unwrap(), b"ABCABCABC");
        // Trailing padding beyond the declared size is ignored.
        let mut padded = with_header(&body);
        padded.extend_from_slice(&[0xFF, 0xFF, 0xFF]);
        assert_eq!(decompress(&padded).unwrap(), b"ABCABCABC");
        // Empty stream.
        assert_eq!(decompress(&with_header(&[])).unwrap(), Vec::<u8>::new());
    }

    // ---- round trips through the test-only compressor ----

    #[test]
    fn round_trip_assorted_inputs() {
        let mut rng = XorShift(0x1234_5678_9ABC_DEF1);
        let mut cases: Vec<Vec<u8>> = vec![
            vec![],
            vec![0],
            b"abc".to_vec(),
            vec![0u8; 5000],
            vec![0xAA; 17],
            (0..=255u8).collect(),
            (0..9000u32).map(|i| (i % 251) as u8).collect(),
        ];
        // Compressible-but-irregular data longer than the 4096 window to exercise ring wrap.
        let mut mixed = Vec::new();
        for i in 0..400u32 {
            let r = rng.next();
            let word = (r & 0x7) as u8;
            mixed.extend_from_slice(&[word, word.wrapping_add(1), (i & 0xFF) as u8, 0, 0, 0, word]);
        }
        cases.push(mixed);
        for (n, data) in cases.iter().enumerate() {
            let packed = compress(data);
            let back = decompress(&packed).unwrap_or_else(|e| panic!("case {n}: {e}"));
            assert_eq!(&back, data, "case {n} did not round-trip");
        }
    }

    // ---- malformed input: errors, never panics ----

    #[test]
    fn rejects_missing_or_inconsistent_header() {
        assert_eq!(decompress(&[]), Err(LzssError::MissingHeader { len: 0 }));
        assert_eq!(decompress(&[1, 0, 0]), Err(LzssError::MissingHeader { len: 3 }));
        // Declares 100 bytes, has 2, and is not the inclusive form either.
        assert!(matches!(
            decompress(&[100, 0, 0, 0, 0xFF, b'x']),
            Err(LzssError::Truncated { declared: 100, available: 2 })
        ));
        // Absurd 4 GiB claim.
        assert!(matches!(decompress(&[0xFF, 0xFF, 0xFF, 0xFF, 0, 0]), Err(LzssError::Truncated { .. })));
    }

    #[test]
    fn a_lone_final_byte_where_a_reference_would_continue_ends_the_stream() {
        // Literal 'Q', then a reference item whose second byte is missing: output so far is kept.
        assert_eq!(decompress(&with_header(&[0x01, b'Q', 0x10])).unwrap(), b"Q");
        // Control says reference first, only one of its two bytes is present: empty output.
        assert_eq!(decompress(&with_header(&[0x00, 0x10])).unwrap(), Vec::<u8>::new());
    }

    #[test]
    fn output_is_capped() {
        // 1000 references of 18 zero bytes each = 18000 bytes.
        // Each group: control byte 0x00 (eight references), then eight 2-byte references of
        // offset 0 (never-written ring: zeros) and length 18.
        let group: Vec<u8> = std::iter::once(0x00).chain([0x00, 0x0F].repeat(8)).collect();
        let body = group.repeat(125);
        let data = with_header(&body);
        assert_eq!(decompress(&data).unwrap().len(), 18_000);
        assert_eq!(decompress_with_limit(&data, 17_999), Err(LzssError::OutputTooLarge { limit: 17_999 }));
        assert_eq!(decompress_with_limit(&data, 0), Err(LzssError::OutputTooLarge { limit: 0 }));
        // Literals are capped too.
        assert_eq!(
            decompress_with_limit(&with_header(&[0xFF, 1, 2, 3, 4, 5, 6, 7, 8]), 5),
            Err(LzssError::OutputTooLarge { limit: 5 })
        );
        // A limit of exactly the output size succeeds.
        assert_eq!(decompress_with_limit(&data, 18_000).unwrap().len(), 18_000);
    }

    #[test]
    fn every_prefix_and_random_garbage_never_panics() {
        let src: Vec<u8> = (0..3000u32).map(|i| ((i * i) % 97) as u8).collect();
        let packed = compress(&src);
        for n in 0..packed.len().min(600) {
            let _ = decompress(packed.get(..n).unwrap_or(&[]));
        }
        let mut rng = XorShift(0xDEAD_BEEF_0BAD_F00D);
        for _ in 0..300 {
            let len = (rng.next() % 200) as usize;
            let mut buf: Vec<u8> = (0..len).map(|_| (rng.next() & 0xFF) as u8).collect();
            if let Some(h) = buf.get_mut(..4) {
                // plausible header so the body decoder actually runs
                let declared = (len.saturating_sub(4)) as u32;
                h.copy_from_slice(&declared.to_le_bytes());
            }
            let _ = decompress_with_limit(&buf, 4096);
        }
    }
}
