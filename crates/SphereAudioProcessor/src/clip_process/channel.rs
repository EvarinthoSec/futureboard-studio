//! Clip-level channel transforms. Allocation-free after construction.

/// How a stereo pair is presented / processed for one clip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChannelTransform {
    #[default]
    Stereo,
    LeftOnly,
    RightOnly,
    MonoSum,
    Swap,
    InvertL,
    InvertR,
    InvertBoth,
    Mid,
    Side,
}

impl ChannelTransform {
    pub const ALL: [Self; 10] = [
        Self::Stereo,
        Self::LeftOnly,
        Self::RightOnly,
        Self::MonoSum,
        Self::Swap,
        Self::InvertL,
        Self::InvertR,
        Self::InvertBoth,
        Self::Mid,
        Self::Side,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Stereo => "Stereo",
            Self::LeftOnly => "Left only",
            Self::RightOnly => "Right only",
            Self::MonoSum => "Mono Sum",
            Self::Swap => "Swap L/R",
            Self::InvertL => "Invert L",
            Self::InvertR => "Invert R",
            Self::InvertBoth => "Invert Both",
            Self::Mid => "Mid",
            Self::Side => "Side",
        }
    }

    pub const fn to_tag(self) -> u8 {
        match self {
            Self::Stereo => 0,
            Self::LeftOnly => 1,
            Self::RightOnly => 2,
            Self::MonoSum => 3,
            Self::Swap => 4,
            Self::InvertL => 5,
            Self::InvertR => 6,
            Self::InvertBoth => 7,
            Self::Mid => 8,
            Self::Side => 9,
        }
    }

    pub const fn from_tag(tag: u8) -> Self {
        match tag {
            1 => Self::LeftOnly,
            2 => Self::RightOnly,
            3 => Self::MonoSum,
            4 => Self::Swap,
            5 => Self::InvertL,
            6 => Self::InvertR,
            7 => Self::InvertBoth,
            8 => Self::Mid,
            9 => Self::Side,
            _ => Self::Stereo,
        }
    }

    pub const fn is_identity(self) -> bool {
        matches!(self, Self::Stereo)
    }
}

/// Apply a channel transform to interleaved PCM (offline / tool-apply path).
pub fn apply_channel_transform_interleaved(
    samples: &[f32],
    channels: usize,
    mode: ChannelTransform,
) -> Vec<f32> {
    if channels < 2 || mode.is_identity() {
        return samples.to_vec();
    }
    let mut out = samples.to_vec();
    for frame in out.chunks_exact_mut(channels) {
        let (left, right) = apply_channel_transform(frame[0], frame[1], mode);
        frame[0] = left;
        frame[1] = right;
    }
    out
}

/// Apply a channel transform to one stereo frame. Realtime-safe.
#[inline]
pub fn apply_channel_transform(left: f32, right: f32, mode: ChannelTransform) -> (f32, f32) {
    match mode {
        ChannelTransform::Stereo => (left, right),
        ChannelTransform::LeftOnly => (left, left),
        ChannelTransform::RightOnly => (right, right),
        ChannelTransform::MonoSum => {
            let m = (left + right) * 0.5;
            (m, m)
        }
        ChannelTransform::Swap => (right, left),
        ChannelTransform::InvertL => (-left, right),
        ChannelTransform::InvertR => (left, -right),
        ChannelTransform::InvertBoth => (-left, -right),
        ChannelTransform::Mid => {
            let m = (left + right) * 0.5;
            (m, m)
        }
        ChannelTransform::Side => {
            let s = (left - right) * 0.5;
            (s, s)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn swap_and_invert_are_exact() {
        assert_eq!(
            apply_channel_transform(0.25, -0.5, ChannelTransform::Swap),
            (-0.5, 0.25)
        );
        assert_eq!(
            apply_channel_transform(0.25, -0.5, ChannelTransform::InvertBoth),
            (-0.25, 0.5)
        );
    }

    #[test]
    fn tags_roundtrip() {
        for mode in ChannelTransform::ALL {
            assert_eq!(ChannelTransform::from_tag(mode.to_tag()), mode);
        }
    }
}
