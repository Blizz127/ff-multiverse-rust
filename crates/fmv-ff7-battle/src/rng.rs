//! Small seeded PRNG (PCG32, XSH-RR 64/32) so server and clients reproduce rolls exactly.
//!
//! Implemented locally (no `rand` dependency) from the public PCG reference:
//! <https://www.pcg-random.org/> (`pcg32_srandom_r` / `pcg32_random_r`). The output sequence
//! is a pure function of `(seed, stream)` on every platform: only wrapping 64-bit integer
//! arithmetic is used.

/// Stream selector used by [`Rng::new`].
pub const DEFAULT_STREAM: u64 = 54;

const MULTIPLIER: u64 = 6_364_136_223_846_793_005;

/// PCG32 generator state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rng {
    state: u64,
    inc: u64,
}

impl Rng {
    /// Generator for `seed` on the default stream.
    pub fn new(seed: u64) -> Self {
        Self::with_stream(seed, DEFAULT_STREAM)
    }

    /// Generator for `seed` on an explicit stream (`pcg32_srandom_r(seed, stream)`).
    pub fn with_stream(seed: u64, stream: u64) -> Self {
        let mut rng = Rng {
            state: 0,
            inc: (stream << 1) | 1,
        };
        rng.next_u32();
        rng.state = rng.state.wrapping_add(seed);
        rng.next_u32();
        rng
    }

    /// Next 32 random bits.
    pub fn next_u32(&mut self) -> u32 {
        let old = self.state;
        self.state = old.wrapping_mul(MULTIPLIER).wrapping_add(self.inc);
        let xorshifted = (((old >> 18) ^ old) >> 27) as u32;
        let rot = (old >> 59) as u32;
        xorshifted.rotate_right(rot)
    }

    /// Next 16 random bits (the high half of a 32-bit draw), like FF7's `rand(0..=65535)`.
    pub fn next_u16(&mut self) -> u16 {
        (self.next_u32() >> 16) as u16
    }

    /// Unbiased value in `0..bound` (rejection sampling, as in `pcg32_boundedrand_r`).
    /// Returns 0 when `bound` is 0.
    pub fn below(&mut self, bound: u32) -> u32 {
        if bound == 0 {
            return 0;
        }
        let threshold = bound.wrapping_neg() % bound;
        loop {
            let r = self.next_u32();
            if r >= threshold {
                return r % bound;
            }
        }
    }

    /// Unbiased value in `lo..=hi`. Returns `lo` when `hi <= lo`.
    pub fn range_inclusive(&mut self, lo: u32, hi: u32) -> u32 {
        if hi <= lo {
            return lo;
        }
        let span = hi - lo;
        match span.checked_add(1) {
            Some(bound) => lo + self.below(bound),
            None => self.next_u32(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reference output of the PCG32 demo for `pcg32_srandom_r(&rng, 42u, 54u)`.
    #[test]
    fn matches_the_pcg32_reference_vector() {
        let mut rng = Rng::with_stream(42, 54);
        let got: Vec<u32> = (0..6).map(|_| rng.next_u32()).collect();
        assert_eq!(
            got,
            [0xa15c02b7, 0x7b47f409, 0xba1d3330, 0x83d2f293, 0xbfa4784b, 0xcbed606e]
        );
        assert_eq!(Rng::new(42), Rng::with_stream(42, DEFAULT_STREAM));
    }

    #[test]
    fn same_seed_same_stream_different_seed_differs() {
        let mut a = Rng::new(7);
        let mut b = Rng::new(7);
        let mut c = Rng::new(8);
        let va: Vec<u32> = (0..16).map(|_| a.next_u32()).collect();
        let vb: Vec<u32> = (0..16).map(|_| b.next_u32()).collect();
        let vc: Vec<u32> = (0..16).map(|_| c.next_u32()).collect();
        assert_eq!(va, vb);
        assert_ne!(va, vc);
    }

    #[test]
    fn bounded_draws_stay_in_range_and_cover_it() {
        let mut rng = Rng::new(1);
        let mut seen = [false; 10];
        for _ in 0..2000 {
            let v = rng.below(10);
            assert!(v < 10);
            seen[v as usize] = true;
        }
        assert!(seen.iter().all(|s| *s), "every value of 0..10 appears");

        let mut lo_hi = (u32::MAX, 0);
        for _ in 0..2000 {
            let v = rng.range_inclusive(3841, 4096);
            assert!((3841..=4096).contains(&v));
            lo_hi = (lo_hi.0.min(v), lo_hi.1.max(v));
        }
        assert!(
            lo_hi.0 < 3860 && lo_hi.1 > 4080,
            "range is exercised: {lo_hi:?}"
        );

        // Degenerate bounds.
        assert_eq!(rng.below(0), 0);
        assert_eq!(rng.below(1), 0);
        assert_eq!(rng.range_inclusive(5, 5), 5);
        assert_eq!(rng.range_inclusive(9, 3), 9);
        let _ = rng.range_inclusive(0, u32::MAX);
        let _ = rng.next_u16();
    }
}
