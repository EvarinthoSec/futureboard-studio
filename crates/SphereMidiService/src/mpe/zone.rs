use super::MpeZoneKind;
use serde::{Deserialize, Serialize};

/// One MPE zone.  Channel fields are MIDI wire channels (0..=15); use
/// [`Self::manager_channel_ui`] and [`Self::member_channel_ui`] for labels.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MpeZone {
    pub kind: MpeZoneKind,
    pub manager_channel: u8,
    pub member_channel_start: u8,
    pub member_channel_end: u8,
    pub member_pitch_range: f32,
    pub manager_pitch_range: f32,
}

impl MpeZone {
    pub fn lower(member_channels: u8) -> Self {
        let count = member_channels.clamp(1, 15);
        Self {
            kind: MpeZoneKind::Lower,
            manager_channel: 0,
            member_channel_start: 1,
            member_channel_end: count,
            member_pitch_range: 48.0,
            manager_pitch_range: 2.0,
        }
    }

    pub fn upper(member_channels: u8) -> Self {
        let count = member_channels.clamp(1, 15);
        Self {
            kind: MpeZoneKind::Upper,
            manager_channel: 15,
            member_channel_start: 15u8.saturating_sub(count),
            member_channel_end: 14,
            member_pitch_range: 48.0,
            manager_pitch_range: 2.0,
        }
    }

    pub fn member_channels(&self) -> Vec<u8> {
        match self.kind {
            MpeZoneKind::Lower => (self.member_channel_start..=self.member_channel_end).collect(),
            MpeZoneKind::Upper => (self.member_channel_start..=self.member_channel_end)
                .rev()
                .collect(),
        }
    }

    pub fn contains_member(&self, channel: u8) -> bool {
        channel != self.manager_channel
            && channel >= self.member_channel_start
            && channel <= self.member_channel_end
    }

    pub fn manager_channel_ui(&self) -> u8 {
        self.manager_channel + 1
    }

    pub fn member_channel_ui(&self) -> (u8, u8) {
        (self.member_channel_start + 1, self.member_channel_end + 1)
    }

    pub fn with_member_channels(mut self, member_channels: u8) -> Self {
        let count = member_channels.clamp(1, 15);
        match self.kind {
            MpeZoneKind::Lower => {
                self.member_channel_start = 1;
                self.member_channel_end = count;
            }
            MpeZoneKind::Upper => {
                self.member_channel_start = 15u8.saturating_sub(count);
                self.member_channel_end = 14;
            }
        }
        self
    }
}

impl Default for MpeZone {
    fn default() -> Self {
        Self::lower(15)
    }
}
