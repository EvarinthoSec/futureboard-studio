//! MIDI Polyphonic Expression transport adapters.
//!
//! MPE is intentionally kept at the input/output boundary.  Decoded data is
//! represented by [`crate::expression::NoteExpression`] and therefore can be
//! sent to a native plug-in expression API or a future MIDI 2.0 UMP adapter.

use serde::{Deserialize, Serialize};

mod allocator;
mod channel_state;
mod configuration;
mod decoder;
mod encoder;
mod note_expression;
mod zone;

pub use allocator::{MpeChannelAllocation, MpeChannelAllocator, MpeChannelExhaustionPolicy};
pub use channel_state::{MpeChannelState, RpnSelection};
pub use configuration::MpeConfiguration;
pub use decoder::{ActiveMpeNote, MpeDecoder, MpeDecoderEvent, MpeMidiMessage};
pub use encoder::{
    choose_expression_route, ClapNoteExpressionOutput, EncodedMidiMessage,
    Midi2NoteExpressionOutput, MpeMidi1Output, NativeNoteExpressionEvent, NoteExpressionOutput,
    NoteExpressionOutputError, NoteExpressionRoute, PluginExpressionCapabilities,
    Vst3NoteExpressionOutput,
};
pub use note_expression::{MpeRecordingSession, RecordedNote};
pub use zone::MpeZone;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MpeZoneKind {
    Lower,
    Upper,
}
