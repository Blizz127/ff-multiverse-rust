//! Pure scene animation-frame selection, shared by presentation and its source validator.

/// A scene-controlled frame. Loops wrap; held poses must name a real frame of the loaded clip.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ClipFrame {
    Loop(f32),
    Hold(u8),
    /// A forward range that holds its last frame rather than looping past it.
    HoldRange { first: u8, last: u8, elapsed: f32 },
    /// Play from frame zero through the loaded clip's actual last frame, then hold it.
    HoldFull { elapsed: f32 },
    /// Play the loaded clip once; completion is resolved against a separate standing default.
    OnceFull { elapsed: f32 },
    /// The same one-shot at two clip frames per scene frame. `elapsed` is clip time;
    /// the standing default resumes at normal speed after the loaded clip completes.
    OnceFullDoubled { elapsed: f32 },
    /// A doubled full Once whose authored ASPED returned to normal at `doubled_until`
    /// clip frames. This preserves both the current pose and the actual completion time.
    OnceFullSlowed { elapsed: f32, doubled_until: f32 },
}

impl ClipFrame {
    pub fn in_clip(self, frames: usize) -> Option<f32> {
        if frames == 0 { return None; }
        match self {
            Self::Loop(frame) => Some(if frame.is_finite() { frame.max(0.0).rem_euclid(frames as f32) } else { 0.0 }),
            Self::Hold(frame) => (usize::from(frame) < frames).then_some(f32::from(frame)),
            Self::HoldRange { first, last, elapsed } => {
                if first > last || usize::from(last) >= frames { return None; }
                let elapsed = if elapsed.is_finite() { elapsed.max(0.0) } else { 0.0 };
                Some(f32::from(first) + elapsed.min(f32::from(last - first)))
            }
            Self::HoldFull { elapsed } => {
                let elapsed = if elapsed.is_finite() { elapsed.max(0.0) } else { 0.0 };
                Some(elapsed.min((frames - 1) as f32))
            }
            Self::OnceFull { elapsed } | Self::OnceFullDoubled { elapsed } | Self::OnceFullSlowed { elapsed, .. } => {
                let elapsed = if elapsed.is_finite() { elapsed.max(0.0) } else { 0.0 };
                (elapsed < frames as f32).then_some(elapsed.min((frames - 1) as f32))
            }
        }
    }

    /// Scene time since a full Once completed its actual loaded clip. A doubled gesture
    /// must not carry its faster clip clock into the separately remembered standing loop.
    pub fn return_elapsed(self, frames: usize) -> Option<f32> {
        if let Self::OnceFullSlowed { elapsed, doubled_until } = self {
            return (frames != 0 && elapsed.is_finite() && doubled_until.is_finite()
                && doubled_until >= 0.0 && elapsed >= doubled_until && elapsed >= frames as f32)
                .then_some(elapsed - frames as f32 - (doubled_until - frames as f32).max(0.0) / 2.0);
        }
        let (elapsed, rate) = match self {
            Self::OnceFull { elapsed } => (elapsed, 1.0),
            Self::OnceFullDoubled { elapsed } => (elapsed, 2.0),
            _ => return None,
        };
        (frames != 0 && elapsed.is_finite() && elapsed >= frames as f32)
            .then_some((elapsed - frames as f32) / rate)
    }
}

