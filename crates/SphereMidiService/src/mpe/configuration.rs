use super::{MpeZone, MpeZoneKind};
use serde::{Deserialize, Serialize};

/// A decoded MPE configuration message.  It is protocol metadata, not note
/// data, and can be surfaced by device detection or a track inspector.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MpeConfiguration {
    pub zone: MpeZoneKind,
    pub member_channels: u8,
    pub member_pitch_range: f32,
    pub manager_pitch_range: f32,
}

impl MpeConfiguration {
    pub fn from_zone(zone: MpeZone) -> Self {
        Self {
            zone: zone.kind,
            member_channels: zone.member_channels().len().min(15) as u8,
            member_pitch_range: zone.member_pitch_range,
            manager_pitch_range: zone.manager_pitch_range,
        }
    }
}
