use crate::expression::{NoteId, PitchExpressionConfig};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RpnSelection {
    pub msb: u8,
    pub lsb: u8,
}

impl Default for RpnSelection {
    fn default() -> Self {
        Self { msb: 127, lsb: 127 }
    }
}

impl RpnSelection {
    pub fn is_null(self) -> bool {
        self.msb == 127 && self.lsb == 127
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MpeChannelState {
    pub active_note: Option<NoteId>,
    /// Normalized -1..=1, with 0 at pitch-bend centre.
    pub pitch_bend: f32,
    /// Normalized 0..=1 channel pressure.
    pub pressure: f32,
    /// Normalized 0..=1 CC74 timbre.
    pub timbre: f32,
    pub sustain: bool,
    pub pitch_config: PitchExpressionConfig,
    pub rpn: RpnSelection,
    pub data_entry_msb: u8,
    pub data_entry_lsb: u8,
}

impl Default for MpeChannelState {
    fn default() -> Self {
        Self {
            active_note: None,
            pitch_bend: 0.0,
            pressure: 0.0,
            timbre: 0.0,
            sustain: false,
            pitch_config: PitchExpressionConfig::default(),
            rpn: RpnSelection::default(),
            data_entry_msb: 0,
            data_entry_lsb: 0,
        }
    }
}
