use super::allocator::MpeChannelAllocator;
use super::zone::MpeZone;
use crate::expression::{NoteExpression, NoteId, PitchExpressionConfig};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncodedMidiMessage {
    pub channel: u8,
    pub bytes: [u8; 3],
}

impl EncodedMidiMessage {
    pub fn new(channel: u8, status: u8, data1: u8, data2: u8) -> Self {
        let channel = channel.min(15);
        Self {
            channel,
            bytes: [status | channel, data1.min(127), data2.min(127)],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteExpressionOutputError {
    NoteNotAllocated,
    ChannelUnavailable,
}

/// Protocol-neutral output contract. Native VST3/CLAP/MIDI 2 adapters can
/// implement this without changing the project note model.
pub trait NoteExpressionOutput {
    fn note_on(
        &mut self,
        note_id: NoteId,
        pitch: u8,
        velocity: f32,
        expression: &NoteExpression,
    ) -> Result<(), NoteExpressionOutputError>;
    fn note_off(
        &mut self,
        note_id: NoteId,
        release_velocity: Option<f32>,
    ) -> Result<(), NoteExpressionOutputError>;
    fn set_pitch(&mut self, note_id: NoteId, value: f32) -> Result<(), NoteExpressionOutputError>;
    fn set_pressure(
        &mut self,
        note_id: NoteId,
        value: f32,
    ) -> Result<(), NoteExpressionOutputError>;
    fn set_timbre(&mut self, note_id: NoteId, value: f32) -> Result<(), NoteExpressionOutputError>;
}

/// MIDI 1.0 MPE output.  `events` is a control/playback buffer owned by the
/// caller; a transport adapter can drain it into its preallocated output ring.
#[derive(Debug, Clone)]
pub struct MpeMidi1Output {
    pub allocator: MpeChannelAllocator,
    pub pitch_config: PitchExpressionConfig,
    events: Vec<EncodedMidiMessage>,
}

impl MpeMidi1Output {
    pub fn new(allocator: MpeChannelAllocator, pitch_range_semitones: f32) -> Self {
        let pitch_range_semitones = if pitch_range_semitones.is_finite() {
            pitch_range_semitones
        } else {
            2.0
        };
        Self {
            allocator,
            pitch_config: PitchExpressionConfig::new(pitch_range_semitones),
            events: Vec::new(),
        }
    }

    pub fn events(&self) -> &[EncodedMidiMessage] {
        &self.events
    }

    pub fn take_events(&mut self) -> Vec<EncodedMidiMessage> {
        std::mem::take(&mut self.events)
    }

    pub fn clear_events(&mut self) {
        self.events.clear();
    }

    /// Reset transport allocations and discard queued output events. The zone
    /// configuration is stateless here and can be sent again explicitly with
    /// [`Self::send_mpe_configuration`].
    pub fn reset(&mut self) {
        self.allocator.reset();
        self.events.clear();
    }

    /// Append the MIDI 1.0 RPN messages needed to configure an MPE zone.
    /// Configuration is explicit because hosts commonly send it once when a
    /// route is enabled, while note expression remains a per-note stream.
    pub fn send_mpe_configuration(&mut self, zone: MpeZone) {
        self.pitch_config = PitchExpressionConfig::new(if zone.member_pitch_range.is_finite() {
            zone.member_pitch_range
        } else {
            2.0
        });
        push_rpn_range(
            &mut self.events,
            zone.manager_channel,
            zone.manager_pitch_range,
        );
        for channel in zone.member_channels() {
            push_rpn_range(&mut self.events, channel, zone.member_pitch_range);
        }
        self.events
            .push(EncodedMidiMessage::new(zone.manager_channel, 0xB0, 101, 0));
        self.events
            .push(EncodedMidiMessage::new(zone.manager_channel, 0xB0, 100, 6));
        self.events.push(EncodedMidiMessage::new(
            zone.manager_channel,
            0xB0,
            6,
            zone.member_channels().len().min(15) as u8,
        ));
    }

    fn emit_pitch(&mut self, channel: u8, value: f32) {
        let value = if value.is_finite() {
            value.clamp(-1.0, 1.0)
        } else {
            0.0
        };
        let value = (((value + 1.0) * 0.5) * 16_383.0).round() as u16;
        self.events.push(EncodedMidiMessage::new(
            channel,
            0xE0,
            (value & 0x7F) as u8,
            (value >> 7) as u8,
        ));
    }

    fn channel_for(&self, note_id: NoteId) -> Result<u8, NoteExpressionOutputError> {
        self.allocator
            .channel_for_note(note_id)
            .ok_or(NoteExpressionOutputError::NoteNotAllocated)
    }
}

fn push_rpn_range(events: &mut Vec<EncodedMidiMessage>, channel: u8, range: f32) {
    let range = if range.is_finite() {
        range.clamp(0.01, 127.99)
    } else {
        2.0
    };
    let msb = range.floor() as u8;
    let lsb = ((range.fract() * 100.0).round() as u8).min(99);
    events.push(EncodedMidiMessage::new(channel, 0xB0, 101, 0));
    events.push(EncodedMidiMessage::new(channel, 0xB0, 100, 0));
    events.push(EncodedMidiMessage::new(channel, 0xB0, 6, msb));
    events.push(EncodedMidiMessage::new(channel, 0xB0, 38, lsb));
}

impl NoteExpressionOutput for MpeMidi1Output {
    fn note_on(
        &mut self,
        note_id: NoteId,
        pitch: u8,
        velocity: f32,
        expression: &NoteExpression,
    ) -> Result<(), NoteExpressionOutputError> {
        let expression = expression.sanitized();
        let allocation = self
            .allocator
            .allocate(
                note_id,
                pitch.min(127),
                (velocity.clamp(0.0, 1.0) * 127.0).round() as u8,
            )
            .ok_or(NoteExpressionOutputError::ChannelUnavailable)?;
        if let Some(stolen_pitch) = allocation.stolen_pitch {
            // A stolen voice is ended before its replacement.  This prevents a
            // held note from receiving the new note's channel expression.
            self.events.push(EncodedMidiMessage::new(
                allocation.channel,
                0x80,
                stolen_pitch,
                0,
            ));
        }
        if allocation.reset_required {
            self.emit_pitch(allocation.channel, 0.0);
            self.events
                .push(EncodedMidiMessage::new(allocation.channel, 0xD0, 0, 0));
            self.events
                .push(EncodedMidiMessage::new(allocation.channel, 0xB0, 74, 0));
        }
        if let Some(point) = expression.pitch.points.first() {
            self.emit_pitch(allocation.channel, point.value);
        }
        self.events.push(EncodedMidiMessage::new(
            allocation.channel,
            0x90,
            pitch.min(127),
            (velocity.clamp(0.0, 1.0) * 127.0).round().max(1.0) as u8,
        ));
        if let Some(point) = expression.pressure.points.first() {
            self.events.push(EncodedMidiMessage::new(
                allocation.channel,
                0xD0,
                (point.value.clamp(0.0, 1.0) * 127.0).round() as u8,
                0,
            ));
        }
        if let Some(point) = expression.timbre.points.first() {
            self.events.push(EncodedMidiMessage::new(
                allocation.channel,
                0xB0,
                74,
                (point.value.clamp(0.0, 1.0) * 127.0).round() as u8,
            ));
        }
        Ok(())
    }

    fn note_off(
        &mut self,
        note_id: NoteId,
        release_velocity: Option<f32>,
    ) -> Result<(), NoteExpressionOutputError> {
        let channel = self.channel_for(note_id)?;
        let pitch = self.allocator.pitch_for_channel(channel).unwrap_or(0);
        let release_velocity = release_velocity
            .filter(|value| value.is_finite())
            .unwrap_or(0.0);
        self.events.push(EncodedMidiMessage::new(
            channel,
            0x80,
            pitch,
            (release_velocity.clamp(0.0, 1.0) * 127.0).round() as u8,
        ));
        self.allocator.release(note_id);
        Ok(())
    }

    fn set_pitch(&mut self, note_id: NoteId, value: f32) -> Result<(), NoteExpressionOutputError> {
        let channel = self.channel_for(note_id)?;
        self.emit_pitch(channel, value);
        Ok(())
    }

    fn set_pressure(
        &mut self,
        note_id: NoteId,
        value: f32,
    ) -> Result<(), NoteExpressionOutputError> {
        let channel = self.channel_for(note_id)?;
        self.events.push(EncodedMidiMessage::new(
            channel,
            0xD0,
            (value.clamp(0.0, 1.0) * 127.0).round() as u8,
            0,
        ));
        Ok(())
    }

    fn set_timbre(&mut self, note_id: NoteId, value: f32) -> Result<(), NoteExpressionOutputError> {
        let channel = self.channel_for(note_id)?;
        self.events.push(EncodedMidiMessage::new(
            channel,
            0xB0,
            74,
            (value.clamp(0.0, 1.0) * 127.0).round() as u8,
        ));
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum NativeNoteExpressionEvent {
    NoteOn {
        note_id: NoteId,
        pitch: u8,
        velocity: f32,
    },
    NoteOff {
        note_id: NoteId,
        release_velocity: Option<f32>,
    },
    Pitch {
        note_id: NoteId,
        value: f32,
    },
    Pressure {
        note_id: NoteId,
        value: f32,
    },
    Timbre {
        note_id: NoteId,
        value: f32,
    },
}

macro_rules! native_output {
    ($name:ident) => {
        #[derive(Debug, Clone, Default)]
        pub struct $name {
            pub events: Vec<NativeNoteExpressionEvent>,
        }

        impl NoteExpressionOutput for $name {
            fn note_on(
                &mut self,
                note_id: NoteId,
                pitch: u8,
                velocity: f32,
                expression: &NoteExpression,
            ) -> Result<(), NoteExpressionOutputError> {
                let expression = expression.sanitized();
                self.events.push(NativeNoteExpressionEvent::NoteOn {
                    note_id,
                    pitch,
                    velocity,
                });
                if let Some(point) = expression.pitch.points.first() {
                    self.events.push(NativeNoteExpressionEvent::Pitch {
                        note_id,
                        value: point.value,
                    });
                }
                if let Some(point) = expression.pressure.points.first() {
                    self.events.push(NativeNoteExpressionEvent::Pressure {
                        note_id,
                        value: point.value,
                    });
                }
                if let Some(point) = expression.timbre.points.first() {
                    self.events.push(NativeNoteExpressionEvent::Timbre {
                        note_id,
                        value: point.value,
                    });
                }
                Ok(())
            }

            fn note_off(
                &mut self,
                note_id: NoteId,
                release_velocity: Option<f32>,
            ) -> Result<(), NoteExpressionOutputError> {
                self.events.push(NativeNoteExpressionEvent::NoteOff {
                    note_id,
                    release_velocity,
                });
                Ok(())
            }

            fn set_pitch(
                &mut self,
                note_id: NoteId,
                value: f32,
            ) -> Result<(), NoteExpressionOutputError> {
                self.events
                    .push(NativeNoteExpressionEvent::Pitch { note_id, value });
                Ok(())
            }

            fn set_pressure(
                &mut self,
                note_id: NoteId,
                value: f32,
            ) -> Result<(), NoteExpressionOutputError> {
                self.events
                    .push(NativeNoteExpressionEvent::Pressure { note_id, value });
                Ok(())
            }

            fn set_timbre(
                &mut self,
                note_id: NoteId,
                value: f32,
            ) -> Result<(), NoteExpressionOutputError> {
                self.events
                    .push(NativeNoteExpressionEvent::Timbre { note_id, value });
                Ok(())
            }
        }
    };
}

// Adapter seam for VST3 Note Expression. The VST3 host can translate these
// note-id events into its native event list without teaching the project
// model about VST3.
native_output!(Vst3NoteExpressionOutput);
// Adapter seam for CLAP note expression.
native_output!(ClapNoteExpressionOutput);
// Adapter seam for the future MIDI 2.0/UMP per-note output.
native_output!(Midi2NoteExpressionOutput);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PluginExpressionCapabilities {
    pub supports_mpe: bool,
    pub supports_native_note_expression: bool,
    pub supports_poly_pressure: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteExpressionRoute {
    Native,
    MpeMidi1,
    /// Explicit user choice to send both native events and an MPE fallback.
    NativeAndMpe,
    StandardMidi,
}

pub fn choose_expression_route(
    capabilities: PluginExpressionCapabilities,
    explicitly_duplicate: bool,
) -> NoteExpressionRoute {
    if capabilities.supports_native_note_expression
        && capabilities.supports_mpe
        && explicitly_duplicate
    {
        NoteExpressionRoute::NativeAndMpe
    } else if capabilities.supports_native_note_expression {
        NoteExpressionRoute::Native
    } else if capabilities.supports_mpe {
        NoteExpressionRoute::MpeMidi1
    } else {
        NoteExpressionRoute::StandardMidi
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::expression::{ExpressionCurve, ExpressionPoint};

    #[test]
    fn output_releases_and_reuses_a_member_channel() {
        let allocator = MpeChannelAllocator::new(1, 1);
        let mut output = MpeMidi1Output::new(allocator, 48.0);
        let expression = NoteExpression {
            pitch: ExpressionCurve::from_points(vec![ExpressionPoint::new(0.0, 0.5)]),
            ..NoteExpression::default()
        };
        output.note_on(1, 60, 0.8, &expression).unwrap();
        output.note_off(1, None).unwrap();
        output
            .note_on(2, 64, 0.8, &NoteExpression::default())
            .unwrap();
        assert_eq!(output.allocator.channel_for_note(2), Some(1));
        assert!(
            output
                .events()
                .iter()
                .any(|event| event.bytes[0] & 0xF0 == 0xE0)
        );
    }

    #[test]
    fn native_expression_is_preferred_without_explicit_duplication() {
        let capabilities = PluginExpressionCapabilities {
            supports_mpe: true,
            supports_native_note_expression: true,
            supports_poly_pressure: false,
        };
        assert_eq!(
            choose_expression_route(capabilities, false),
            NoteExpressionRoute::Native
        );
        assert_eq!(
            choose_expression_route(capabilities, true),
            NoteExpressionRoute::NativeAndMpe
        );
    }

    #[test]
    fn configuration_emits_rpn_range_and_member_count() {
        let mut output = MpeMidi1Output::new(MpeChannelAllocator::new(1, 2), 48.0);
        output.send_mpe_configuration(MpeZone::lower(2));
        assert!(output.events().iter().any(|event| {
            event.channel == 0 && event.bytes[0] & 0xF0 == 0xB0 && event.bytes[1..] == [6, 2]
        }));
        assert!(output.events().iter().any(|event| {
            event.channel == 1 && event.bytes[0] & 0xF0 == 0xB0 && event.bytes[1..] == [6, 48]
        }));
    }
}
