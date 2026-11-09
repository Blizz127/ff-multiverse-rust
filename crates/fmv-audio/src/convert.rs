//! From the mixer's interleaved stereo `f32` to whatever a device wants: its sample format and its
//! number of channels. Pure functions, so they are tested here without a device; the device glue
//! around them stays thin.

/// A sample format a device can take.
pub trait DeviceSample: Copy {
    /// `v` in `-1.0..=1.0` as this format (anything outside is clipped, a NaN is silence).
    fn from_f32(v: f32) -> Self;
}

impl DeviceSample for f32 {
    fn from_f32(v: f32) -> Self {
        if v.is_nan() {
            0.0
        } else {
            v.clamp(-1.0, 1.0)
        }
    }
}

impl DeviceSample for i16 {
    fn from_f32(v: f32) -> Self {
        // `as` from a float saturates and turns NaN into zero.
        (v.clamp(-1.0, 1.0) * f32::from(i16::MAX)).round() as i16
    }
}

impl DeviceSample for u16 {
    /// Unsigned 16-bit audio is centred on 32768.
    fn from_f32(v: f32) -> Self {
        let centred = f64::from(if v.is_nan() { 0.0 } else { v.clamp(-1.0, 1.0) }) * f64::from(i16::MAX);
        (centred + 32_768.0).round() as u16
    }
}

impl DeviceSample for i32 {
    fn from_f32(v: f32) -> Self {
        (f64::from(if v.is_nan() { 0.0 } else { v.clamp(-1.0, 1.0) }) * f64::from(i32::MAX)).round() as i32
    }
}

/// Write the stereo frames of `stereo` into `out`, which has `channels` samples per frame: mono
/// gets the average of both sides, stereo is copied, and a device with more channels gets the
/// front pair on its first two and silence on the rest. Frames past what either side holds are left
/// alone (the caller sizes both).
pub fn write_frames<T: DeviceSample>(stereo: &[f32], channels: usize, out: &mut [T]) {
    if channels == 0 {
        return;
    }
    for (frame, target) in stereo.as_chunks::<2>().0.iter().zip(out.chunks_exact_mut(channels)) {
        let (l, r) = (frame[0], frame[1]);
        match target {
            [only] => *only = T::from_f32((l + r) * 0.5),
            [left, right, rest @ ..] => {
                *left = T::from_f32(l);
                *right = T::from_f32(r);
                rest.fill(T::from_f32(0.0));
            }
            [] => {}
        }
    }
}

/// Fill a device buffer of `out.len() / channels` frames by rendering stereo in chunks of at most
/// `scratch.len() / 2` frames (so a callback of any size needs no allocation) and converting each.
/// `render` is the mixer. A trailing partial frame, if the buffer has one, is silence.
pub fn fill_device<T: DeviceSample>(out: &mut [T], channels: usize, scratch: &mut [f32], mut render: impl FnMut(&mut [f32])) {
    let silence = T::from_f32(0.0);
    let per_chunk = scratch.len() / 2;
    if channels == 0 || per_chunk == 0 {
        out.fill(silence);
        return;
    }
    let frames = out.len() / channels;
    let mut done = 0;
    while done < frames {
        let n = (frames - done).min(per_chunk);
        let stereo = &mut scratch[..n * 2];
        render(stereo);
        write_frames(stereo, channels, &mut out[done * channels..(done + n) * channels]);
        done += n;
    }
    out[frames * channels..].fill(silence);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sixteen_bit_conversion_rounds_clips_and_zeroes_nan() {
        assert_eq!(i16::from_f32(0.0), 0);
        assert_eq!(i16::from_f32(1.0), i16::MAX);
        assert_eq!(i16::from_f32(-1.0), -i16::MAX);
        assert_eq!(i16::from_f32(5.0), i16::MAX);
        assert_eq!(i16::from_f32(-5.0), -i16::MAX);
        assert_eq!(i16::from_f32(f32::NAN), 0);
        assert_eq!(i16::from_f32(0.5), 16_384, "16383.5 rounds away from zero");
        assert_eq!(u16::from_f32(0.0), 32_768);
        assert_eq!(u16::from_f32(1.0), 65_535);
        assert_eq!(u16::from_f32(-1.0), 1);
        assert_eq!(u16::from_f32(f32::NAN), 32_768);
        assert_eq!(i32::from_f32(1.0), i32::MAX);
        assert_eq!(i32::from_f32(-2.0), -i32::MAX);
        assert_eq!(i32::from_f32(f32::NAN), 0);
        assert_eq!(f32::from_f32(0.25), 0.25);
        assert_eq!(f32::from_f32(3.0), 1.0);
        assert_eq!(f32::from_f32(f32::NAN), 0.0);
    }

    #[test]
    fn frames_are_mapped_onto_any_number_of_channels() {
        let stereo = [0.5f32, -0.25, 1.0, 0.0];
        let mut mono = [9.0f32; 2];
        write_frames(&stereo, 1, &mut mono);
        assert_eq!(mono, [0.125, 0.5]);
        let mut two = [9.0f32; 4];
        write_frames(&stereo, 2, &mut two);
        assert_eq!(two, stereo);
        let mut six = [9.0f32; 12];
        write_frames(&stereo, 6, &mut six);
        assert_eq!(six, [0.5, -0.25, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0]);
        let mut none = [9.0f32; 2];
        write_frames(&stereo, 0, &mut none);
        assert_eq!(none, [9.0, 9.0]);
        // A device buffer shorter than the mixer's: only the frames that fit are written.
        let mut short = [9.0f32; 2];
        write_frames(&stereo, 2, &mut short);
        assert_eq!(short, [0.5, -0.25]);
    }

    #[test]
    fn a_callback_larger_than_the_scratch_is_filled_in_chunks_in_order() {
        // A "mixer" that numbers its frames across calls, so order and continuity are visible.
        let mut next = 0.0f32;
        let mut scratch = [0.0f32; 6]; // three frames per chunk
        let mut out = [0i16; 2 * 8 + 1]; // eight stereo frames and a stray sample
        fill_device(&mut out, 2, &mut scratch, |buf| {
            for frame in buf.as_chunks_mut::<2>().0.iter_mut() {
                frame.fill(next);
                next += 0.001;
            }
        });
        let expect: Vec<i16> = (0..8).flat_map(|k| [i16::from_f32(k as f32 * 0.001); 2]).chain([0]).collect();
        assert_eq!(out.to_vec(), expect);
    }

    #[test]
    fn a_degenerate_device_buffer_is_silence_and_never_panics() {
        let mut out = [7i16; 5];
        fill_device(&mut out, 0, &mut [0.0; 4], |_| unreachable!("no channels, nothing to render"));
        assert_eq!(out, [0; 5]);
        let mut out = [7i16; 5];
        fill_device(&mut out, 2, &mut [], |_| unreachable!("no scratch, nothing to render"));
        assert_eq!(out, [0; 5]);
        let mut empty: [i16; 0] = [];
        fill_device(&mut empty, 2, &mut [0.0; 4], |_| unreachable!("no frames"));
    }
}
