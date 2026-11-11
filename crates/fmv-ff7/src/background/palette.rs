//! Palette work performed by field scripts, before a tile is expanded to RGBA.
//!
//! Scratch positions are 16-color offsets into one 1024-word buffer, cleared on field entry.
//! Store reads the current live palette; copy and multiply change only scratch; load publishes
//! scratch colors to the live palette. Counts here are resolved lengths: the encoded byte plus
//! one, so 255 means 256 colors. Overlapping copy/multiply spans are processed forwards, without
//! a source snapshot. All spans are validated before any mutation.
//!
//! Primary evidence (semantics only; no implementation copied): original-engine
//! [palette handlers](https://github.com/Akari1982/q-gears_reverse/blob/e0fabf7e121818e5abe2f873e34a07d0eb32742d/ffvii/DISC/FIELD/FIELD.BIN_event_codes_palette.cpp#L1-L127),
//! [copy/count assembly](https://github.com/Akari1982/q-gears_reverse/blob/e0fabf7e121818e5abe2f873e34a07d0eb32742d/ffvii/DISC/FIELD/FIELD.BIN__clear.asm#L50552-L50609),
//! and [multiply assembly](https://github.com/Akari1982/q-gears_reverse/blob/e0fabf7e121818e5abe2f873e34a07d0eb32742d/ffvii/DISC/FIELD/FIELD.BIN__clear.asm#L51401-L51461).
//! The assembly establishes widened counts and the adjacent-channel bits in multiplication;
//! these details must not be simplified to an independent RGB multiply. The evidence is from
//! the PSX engine. FFNx's [PC opcode table](https://github.com/julianxhokaxhiu/FFNx/blob/661f92a36c3f1d6498317e04327c31dc24d06222/src/ff7/field/opcode.h#L136-L141)
//! agrees on opcode identity, but retains original handlers, so PC scratch layout/arithmetic
//! have not been independently verified. PC transparency remains [`Palette`]'s index-zero rule.

use crate::field::Palette;
use std::ops::Range;

/// A resolved palette instruction. `count` is the number of colors, in 1..=256, rather than
/// the encoded last index. Source/destination positions address 16-color scratch units.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaletteOp {
    Store { palette: u8, position: u8, count: u16 },
    Load { position: u8, palette: u8, count: u16 },
    Copy { source: u8, destination: u8, count: u16 },
    Multiply { source: u8, destination: u8, red: u8, green: u8, blue: u8, count: u16 },
}

/// A field's live colors and its contiguous script scratch buffer. Start a new state on field
/// entry; keep it across scene boundaries in that field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaletteState {
    live: Palette,
    scratch: [u16; 1024],
}

impl PaletteState {
    pub fn new(palette: Palette) -> Self {
        Self { live: palette, scratch: [0; 1024] }
    }

    /// The current live palette, with the normal PC transparency rule unchanged.
    pub fn palette(&self) -> &Palette {
        &self.live
    }

    /// Apply one instruction. Returns whether live colors changed; scratch-only work returns
    /// false even when it changes scratch. An invalid count, page or full span changes nothing.
    pub fn apply(&mut self, op: PaletteOp) -> Result<bool, String> {
        let count = match op {
            PaletteOp::Store { count, .. } | PaletteOp::Load { count, .. }
            | PaletteOp::Copy { count, .. } | PaletteOp::Multiply { count, .. } => count,
        };
        if !(1..=256).contains(&count) {
            return Err(format!("palette count {count} is outside 1..=256"));
        }
        let count = usize::from(count);
        match op {
            PaletteOp::Store { palette, position, .. } => {
                self.check_palette(palette, count)?;
                let to = scratch_range(position, count)?;
                for (index, target) in to.enumerate() {
                    // The full page/span was checked before any scratch entry changes.
                    let target = self.scratch.get_mut(target).ok_or("invalid palette scratch destination")?;
                    *target = self.live.raw(usize::from(palette), index).unwrap_or(0);
                }
                Ok(false)
            }
            PaletteOp::Load { position, palette, .. } => {
                let from = scratch_range(position, count)?;
                self.check_palette(palette, count)?;
                let mut changed = false;
                for (index, source) in from.enumerate() {
                    let color = self.scratch.get(source).copied().ok_or("invalid palette scratch source")?;
                    changed |= self.live.set_raw(usize::from(palette), index, color).unwrap_or(false);
                }
                Ok(changed)
            }
            PaletteOp::Copy { source, destination, .. } => {
                let from = scratch_range(source, count)?;
                let to = scratch_range(destination, count)?;
                for (source, destination) in from.zip(to) {
                    let color = self.scratch.get(source).copied().ok_or("invalid palette scratch source")?;
                    let target = self.scratch.get_mut(destination).ok_or("invalid palette scratch destination")?;
                    *target = color;
                }
                Ok(false)
            }
            PaletteOp::Multiply { source, destination, red, green, blue, .. } => {
                let from = scratch_range(source, count)?;
                let to = scratch_range(destination, count)?;
                for (source, destination) in from.zip(to) {
                    let color = self.scratch.get(source).copied().ok_or("invalid palette scratch source")?;
                    if color != 0 {
                        let target = self.scratch.get_mut(destination).ok_or("invalid palette scratch destination")?;
                        *target = multiplied(color, red, green, blue);
                    }
                }
                Ok(false)
            }
        }
    }

    fn check_palette(&self, page: u8, count: usize) -> Result<(), String> {
        if usize::from(page) >= self.live.page_count() || count > usize::from(self.live.colors_per_page) {
            return Err(format!("palette page {page} does not contain {count} colors"));
        }
        Ok(())
    }
}

fn scratch_range(position: u8, count: usize) -> Result<Range<usize>, String> {
    let start = usize::from(position) * 16;
    let end = start + count;
    if position >= 64 || end > 1024 {
        return Err(format!("palette scratch position {position} cannot hold {count} colors"));
    }
    Ok(start..end)
}

fn multiplied(color: u16, red: u8, green: u8, blue: u8) -> u16 {
    // Original byte scales are unsigned; 64 is neutral. The green/blue inputs include the
    // preceding channel's high bit, as the engine's shifts and masks do.
    let scale = |value: u16, factor: u8| ((u32::from(value) * u32::from(factor)) >> 7).min(31) as u16;
    let r = scale((color << 1) & 0x3E, red);
    let g = scale((color >> 4) & 0x3F, green);
    let b = scale((color >> 9) & 0x3F, blue);
    let out = (color & 0x8000) | r | (g << 5) | (b << 10);
    if out == 0 { 0x8000 } else { out }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn palette(pages: &[Vec<u16>]) -> Palette {
        let width = pages.first().map_or(256, Vec::len);
        assert!(pages.iter().all(|page| page.len() == width));
        let mut bytes = vec![0; 12];
        bytes[8..10].copy_from_slice(&(width as u16).to_le_bytes());
        bytes[10..12].copy_from_slice(&(pages.len() as u16).to_le_bytes());
        for color in pages.iter().flatten() { bytes.extend_from_slice(&color.to_le_bytes()); }
        Palette::parse(&bytes).unwrap()
    }

    fn page(state: &PaletteState, page: usize) -> Vec<u16> {
        (0..usize::from(state.palette().colors_per_page)).map(|i| state.palette().raw(page, i).unwrap()).collect()
    }

    #[test]
    fn scratch_starts_zero_and_256_colors_fit_exactly_in_the_final_sixteen_positions() {
        let colors: Vec<_> = (1..=256).collect();
        let mut state = PaletteState::new(palette(&[colors.clone(), vec![0x1234; 256]]));
        assert!(state.apply(PaletteOp::Load { position: 0, palette: 1, count: 256 }).unwrap());
        assert_eq!(page(&state, 1), vec![0; 256]);
        assert!(!state.apply(PaletteOp::Store { palette: 0, position: 48, count: 256 }).unwrap());
        assert!(state.apply(PaletteOp::Load { position: 48, palette: 1, count: 256 }).unwrap());
        assert_eq!(page(&state, 1), colors, "the final color of the 256-entry range is copied too");
        assert!(!state.apply(PaletteOp::Load { position: 48, palette: 1, count: 256 }).unwrap());
        assert!(state.apply(PaletteOp::Load { position: 63, palette: 1, count: 16 }).unwrap());
        assert_eq!(&page(&state, 1)[..16], &(241..=256).collect::<Vec<_>>());
    }

    #[test]
    fn invalid_operations_leave_both_live_colors_and_scratch_unchanged() {
        let mut state = PaletteState::new(palette(&[vec![0x1234; 256]]));
        state.apply(PaletteOp::Store { palette: 0, position: 0, count: 256 }).unwrap();
        for op in [
            PaletteOp::Store { palette: 1, position: 0, count: 1 },
            PaletteOp::Store { palette: 0, position: 63, count: 17 },
            PaletteOp::Store { palette: 0, position: 0, count: 0 },
            PaletteOp::Load { position: 0, palette: 1, count: 1 },
            PaletteOp::Load { position: 49, palette: 0, count: 256 },
            PaletteOp::Load { position: 0, palette: 0, count: 257 },
            PaletteOp::Copy { source: 64, destination: 0, count: 1 },
            PaletteOp::Copy { source: 0, destination: 63, count: 17 },
            PaletteOp::Multiply { source: 0, destination: 255, red: 0, green: 0, blue: 0, count: 1 },
            PaletteOp::Multiply { source: 63, destination: 0, red: 0, green: 0, blue: 0, count: 17 },
        ] {
            let before = state.clone();
            assert!(state.apply(op).is_err(), "{op:?}");
            assert_eq!(state, before, "invalid source or destination must not partially apply: {op:?}");
        }
        let mut short = PaletteState::new(palette(&[vec![0x1234; 16]]));
        let before = short.clone();
        assert!(short.apply(PaletteOp::Store { palette: 0, position: 0, count: 17 }).is_err());
        assert!(short.apply(PaletteOp::Load { position: 0, palette: 0, count: 17 }).is_err());
        assert_eq!(short, before);
    }

    #[test]
    fn overlapping_copy_uses_forward_writes_and_only_load_publishes_them() {
        let colors: Vec<_> = (1..=256).collect();
        let mut state = PaletteState::new(palette(&[colors.clone(), vec![0x1234; 256]]));
        state.apply(PaletteOp::Store { palette: 0, position: 0, count: 256 }).unwrap();
        assert!(!state.apply(PaletteOp::Copy { source: 0, destination: 1, count: 32 }).unwrap());
        assert_eq!(page(&state, 0), colors);
        assert_eq!(page(&state, 1), vec![0x1234; 256], "scratch work is not displayed yet");
        assert!(state.apply(PaletteOp::Load { position: 1, palette: 1, count: 32 }).unwrap());
        let first: Vec<u16> = (1..=16).collect();
        assert_eq!(&page(&state, 1)[..32], &[first.clone(), first].concat(), "the overlapping source sees earlier writes");
        assert_eq!(state.palette().raw(1, 32), Some(0x1234), "a partial load preserves colors beyond the count");
    }

    #[test]
    fn overlapping_multiply_reads_the_words_it_just_wrote() {
        let mut state = PaletteState::new(palette(&[vec![1; 256], vec![0; 256]]));
        state.apply(PaletteOp::Store { palette: 0, position: 0, count: 256 }).unwrap();
        assert!(!state.apply(PaletteOp::Multiply { source: 0, destination: 1, red: 128, green: 64, blue: 64, count: 32 }).unwrap());
        state.apply(PaletteOp::Load { position: 1, palette: 1, count: 32 }).unwrap();
        assert_eq!(&page(&state, 1)[..16], &[2; 16]);
        assert_eq!(&page(&state, 1)[16..32], &[4; 16], "the overlap multiplies already doubled words again");
    }

    #[test]
    fn multiply_matches_raw_channel_bits_unsigned_scales_and_high_bit() {
        let input = vec![0x0010, 0x0200, 0x7FFF, 0x8001, 0x1234];
        let mut state = PaletteState::new(palette(&[input.clone(), vec![0; input.len()]]));
        state.apply(PaletteOp::Store { palette: 0, position: 0, count: 5 }).unwrap();
        state.apply(PaletteOp::Multiply { source: 0, destination: 16, red: 64, green: 64, blue: 64, count: 5 }).unwrap();
        state.apply(PaletteOp::Load { position: 16, palette: 1, count: 5 }).unwrap();
        assert_eq!(page(&state, 1), input, "scale 64 is identity");
        state.apply(PaletteOp::Multiply { source: 0, destination: 16, red: 128, green: 128, blue: 128, count: 4 }).unwrap();
        state.apply(PaletteOp::Load { position: 16, palette: 1, count: 4 }).unwrap();
        assert_eq!(&page(&state, 1)[..4], &[0x003F, 0x07E0, 0x7FFF, 0x8002], "green/blue include the preceding channel's top bit");
        state.apply(PaletteOp::Multiply { source: 0, destination: 16, red: 255, green: 0, blue: 0, count: 4 }).unwrap();
        state.apply(PaletteOp::Load { position: 16, palette: 1, count: 4 }).unwrap();
        assert_eq!(state.palette().raw(1, 3), Some(0x8003), "255 is a positive scale, not signed -1");
    }

    #[test]
    fn zero_source_skips_the_destination_and_nonzero_black_preserves_opacity_metadata() {
        let mut state = PaletteState::new(palette(&[vec![0, 0x8000, 1], vec![0x1234; 3]]));
        state.apply(PaletteOp::Store { palette: 0, position: 0, count: 3 }).unwrap();
        state.apply(PaletteOp::Store { palette: 1, position: 16, count: 3 }).unwrap();
        state.apply(PaletteOp::Multiply { source: 0, destination: 16, red: 0, green: 0, blue: 0, count: 3 }).unwrap();
        state.apply(PaletteOp::Load { position: 16, palette: 1, count: 3 }).unwrap();
        assert_eq!(page(&state, 1), [0x1234, 0x8000, 0x8000]);
        assert_eq!(state.palette().rgba(1, 0).unwrap()[3], 0, "PC index zero stays transparent even with nonzero color");
        assert_eq!(state.palette().rgba(1, 2), Some([0, 0, 0, 255]));
    }

    #[test]
    fn store_reads_the_current_live_palette_after_an_earlier_load() {
        let mut state = PaletteState::new(palette(&[vec![1; 256], vec![0x1234; 256]]));
        state.apply(PaletteOp::Store { palette: 0, position: 0, count: 256 }).unwrap();
        state.apply(PaletteOp::Multiply { source: 0, destination: 16, red: 0, green: 0, blue: 0, count: 256 }).unwrap();
        assert_eq!(page(&state, 0), vec![1; 256]);
        assert!(state.apply(PaletteOp::Load { position: 16, palette: 0, count: 256 }).unwrap());
        state.apply(PaletteOp::Store { palette: 0, position: 32, count: 256 }).unwrap();
        state.apply(PaletteOp::Load { position: 32, palette: 1, count: 256 }).unwrap();
        assert_eq!(page(&state, 0), vec![0x8000; 256]);
        assert_eq!(page(&state, 1), vec![0x8000; 256], "store captured the displayed result, not the original file");
    }
}
