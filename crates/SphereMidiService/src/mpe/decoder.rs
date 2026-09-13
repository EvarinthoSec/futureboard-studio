use super::channel_state::MpeChannelState;
use super::configuration::MpeConfiguration;
use super::zone::MpeZone;
use crate::MidiInputEvent;
use crate::expression::{
    ExpressionInterpolation, ExpressionPoint, NoteExpression, NoteExpressionLane, NoteId, Tick,
};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_NOTE_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MpeMidiMessage {
    NoteOn {
        channel: u8,
        note: u8,
        velocity: u8,
    },
    NoteOff {
        channel: u8,
        note: u8,
        release_velocity: u8,
    },
    PitchBend {
        channel: u8,
        value: u16,
    },
    ChannelPressure {
        channel: u8,
        value: u8,
    },
    PolyPressure {
        channel: u8,
        note: u8,
        value: u8,
    },
    ControlChange {
        channel: u8,
        controller: u8,
        value: u8,
    },
    AllNotesOff,
}

impl From<&MidiInputEvent> for Option<MpeMidiMessage> {
    fn from(event: &MidiInputEvent) -> Self {
        match *event {
            MidiInputEvent::NoteOn {
                note,
                velocity,
                channel,
            } => Some(MpeMidiMessage::NoteOn {
                channel,
                note,
                velocity,
            }),
            MidiInputEvent::NoteOff { note, channel } => Some(MpeMidiMessage::NoteOff {
                channel,
                note,
                release_velocity: 0,
            }),
            MidiInputEvent::PitchBend { channel, value } => {
                Some(MpeMidiMessage::PitchBend { channel, value })
            }
            MidiInputEvent::ChannelPressure { channel, value } => {
                Some(MpeMidiMessage::ChannelPressure { channel, value })
            }
            MidiInputEvent::PolyPressure {
                note,
                channel,
                value,
            } => Some(MpeMidiMessage::PolyPressure {
                channel,
                note,
                value,
            }),
            MidiInputEvent::ControlChange {
                controller,
                value,
                channel,
            } => Some(MpeMidiMessage::ControlChange {
                channel,
                controller,
                value,
            }),
            MidiInputEvent::AllNotesOff | MidiInputEvent::Panic => {
                Some(MpeMidiMessage::AllNotesOff)
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum MpeDecoderEvent {
    NoteOn {
        note_id: NoteId,
        channel: u8,
        pitch: u8,
        velocity: u8,
        position: Tick,
        expression: NoteExpression,
    },
    Expression {
        note_id: NoteId,
        lane: NoteExpressionLane,
        point: ExpressionPoint,
    },
    NoteOff {
        note_id: NoteId,
        channel: u8,
        pitch: u8,
        position: Tick,
        release_velocity: Option<f32>,
    },
    ConfigurationChanged(MpeConfiguration),
}

#[derive(Debug, Clone)]
pub struct ActiveMpeNote {
    pub note_id: NoteId,
    pub channel: u8,
    pub pitch: u8,
    pub velocity: u8,
    pub start: Tick,
    pub key_down: bool,
    pub expression: NoteExpression,
}

/// MPE decoder with independent state for every MIDI channel.  The channel is
/// used to find the live transport voice, then the generated note id becomes
/// the identity of all expression points.
#[derive(Debug, Clone)]
pub struct MpeDecoder {
    zones: Vec<MpeZone>,
    channels: [MpeChannelState; 16],
    /// The required active-note map is keyed by transport channel only while a
    /// note is live. It is never serialized into a project note.
    active_notes: HashMap<u8, ActiveMpeNote>,
}

impl Default for MpeDecoder {
    fn default() -> Self {
        Self::new(vec![MpeZone::lower(15)])
    }
}

impl MpeDecoder {
    pub fn new(zones: Vec<MpeZone>) -> Self {
        let mut decoder = Self {
            zones: Vec::new(),
            channels: [MpeChannelState::default(); 16],
            active_notes: HashMap::new(),
        };
        for zone in zones {
            decoder.set_zone(zone);
        }
        decoder
    }

    pub fn zones(&self) -> &[MpeZone] {
        &self.zones
    }

    pub fn set_zone(&mut self, zone: MpeZone) {
        if let Some(existing) = self.zones.iter_mut().find(|item| item.kind == zone.kind) {
            *existing = zone;
        } else if self.zones.len() < 2 {
            self.zones.push(zone);
        }
        self.apply_zone_pitch_ranges();
    }

    pub fn channel_state(&self, channel: u8) -> &MpeChannelState {
        &self.channels[channel.min(15) as usize]
    }

    pub fn active_notes(&self) -> &HashMap<u8, ActiveMpeNote> {
        &self.active_notes
    }

    pub fn pitch_range_for(&self, channel: u8) -> f32 {
        self.channel_state(channel).pitch_config.range_semitones
    }

    /// Drop live note associations and return every channel to its default
    /// controller state while preserving the configured zones. Used by a
    /// panic/reset action before accepting a new MPE stream.
    pub fn reset(&mut self) {
        self.active_notes.clear();
        self.channels = [MpeChannelState::default(); 16];
        self.apply_zone_pitch_ranges();
    }

    pub fn process_input(
        &mut self,
        event: &MidiInputEvent,
        position: Tick,
    ) -> Vec<MpeDecoderEvent> {
        let Some(message) = Option::<MpeMidiMessage>::from(event) else {
            return Vec::new();
        };
        self.process(message, position)
    }

    pub fn process(&mut self, message: MpeMidiMessage, position: Tick) -> Vec<MpeDecoderEvent> {
        let position = position.max(0.0);
        match message {
            MpeMidiMessage::NoteOn {
                channel,
                note,
                velocity: 0,
            } => self.note_off(channel, note, 0, position),
            MpeMidiMessage::NoteOn {
                channel,
                note,
                velocity,
            } => self.note_on(channel, note, velocity, position),
            MpeMidiMessage::NoteOff {
                channel,
                note,
                release_velocity,
            } => self.note_off(channel, note, release_velocity, position),
            MpeMidiMessage::PitchBend { channel, value } => self.update_expression(
                channel,
                NoteExpressionLane::Pitch,
                bend_to_normalized(value),
                position,
            ),
            MpeMidiMessage::ChannelPressure { channel, value } => self.update_expression(
                channel,
                NoteExpressionLane::Pressure,
                value as f32 / 127.0,
                position,
            ),
            // Poly pressure already has a note association, so it can feed
            // the same note-owned pressure lane without changing the model.
            MpeMidiMessage::PolyPressure {
                channel,
                note,
                value,
            } => self.update_poly_pressure(channel, note, value, position),
            MpeMidiMessage::ControlChange {
                channel,
                controller,
                value,
            } => self.control_change(channel, controller, value, position),
            MpeMidiMessage::AllNotesOff => self.finish_all(position),
        }
    }

    pub fn finish(&mut self, position: Tick) -> Vec<MpeDecoderEvent> {
        self.finish_all(position.max(0.0))
    }

    fn note_on(
        &mut self,
        channel: u8,
        note: u8,
        velocity: u8,
        position: Tick,
    ) -> Vec<MpeDecoderEvent> {
        let channel = channel.min(15);
        let mut events = Vec::new();
        if let Some(previous) = self.active_notes.remove(&channel) {
            self.channels[channel as usize].active_note = None;
            events.push(note_off_event(previous, position, None));
        }
        let state = *self.channel_state(channel);
        let note_id = NEXT_NOTE_ID.fetch_add(1, Ordering::Relaxed);
        let expression = NoteExpression {
            // A default channel state is not expression data. Only retain a
            // pre-note value when the controller actually moved before the
            // Note On; this prevents a plain note or sustain pedal from
            // turning every recorded note into a three-curve MPE note.
            pitch: (state.pitch_bend.abs() > f32::EPSILON)
                .then(|| curve_with_initial(state.pitch_bend))
                .unwrap_or_default(),
            pressure: (state.pressure > f32::EPSILON)
                .then(|| curve_with_initial(state.pressure))
                .unwrap_or_default(),
            timbre: (state.timbre > f32::EPSILON)
                .then(|| curve_with_initial(state.timbre))
                .unwrap_or_default(),
            ..NoteExpression::default()
        };
        let active = ActiveMpeNote {
            note_id,
            channel,
            pitch: note.min(127),
            velocity: velocity.max(1),
            start: position,
            key_down: true,
            expression: expression.clone(),
        };
        self.active_notes.insert(channel, active);
        self.channels[channel as usize].active_note = Some(note_id);
        events.push(MpeDecoderEvent::NoteOn {
            note_id,
            channel,
            pitch: note.min(127),
            velocity: velocity.max(1),
            position,
            expression,
        });
        events
    }

    fn note_off(
        &mut self,
        channel: u8,
        note: u8,
        release_velocity: u8,
        position: Tick,
    ) -> Vec<MpeDecoderEvent> {
        let channel = channel.min(15);
        let Some(active) = self.active_notes.get_mut(&channel) else {
            return Vec::new();
        };
        if active.pitch != note.min(127) {
            return Vec::new();
        }
        active.key_down = false;
        if self.channels[channel as usize].sustain {
            return Vec::new();
        }
        let active = self.active_notes.remove(&channel).expect("checked above");
        self.channels[channel as usize].active_note = None;
        vec![note_off_event(
            active,
            position,
            (release_velocity > 0).then_some(release_velocity as f32 / 127.0),
        )]
    }

    fn control_change(
        &mut self,
        channel: u8,
        controller: u8,
        value: u8,
        position: Tick,
    ) -> Vec<MpeDecoderEvent> {
        let channel = channel.min(15);
        match controller {
            64 => {
                let is_down = value >= 64;
                let channels = self.sustain_channels(channel);
                let mut events = Vec::new();
                for channel in channels {
                    let was_down = self.channels[channel as usize].sustain;
                    self.channels[channel as usize].sustain = is_down;
                    if was_down && !is_down {
                        events.extend(self.release_sustained(channel, position));
                    }
                }
                events
            }
            74 => self.update_expression(
                channel,
                NoteExpressionLane::Timbre,
                value as f32 / 127.0,
                position,
            ),
            101 => {
                self.channels[channel as usize].rpn.msb = value.min(127);
                Vec::new()
            }
            100 => {
                self.channels[channel as usize].rpn.lsb = value.min(127);
                Vec::new()
            }
            6 => self.data_entry(channel, value, false),
            38 => self.data_entry(channel, value, true),
            // All Sound Off and All Notes Off both terminate live voices for
            // recording purposes. Devices commonly use either panic message;
            // treating only CC123 as a terminator leaves notes stuck when CC120
            // is the one they actually send.
            120 | 123 => self.finish_all(position),
            _ => Vec::new(),
        }
    }

    fn data_entry(&mut self, channel: u8, value: u8, lsb: bool) -> Vec<MpeDecoderEvent> {
        let (rpn, data_entry_msb, data_entry_lsb) = {
            let state = &mut self.channels[channel as usize];
            if lsb {
                state.data_entry_lsb = value.min(127);
            } else {
                state.data_entry_msb = value.min(127);
            }
            (state.rpn, state.data_entry_msb, state.data_entry_lsb)
        };
        if rpn.msb == 0 && rpn.lsb == 0 {
            let range = data_entry_msb as f32 + data_entry_lsb.min(99) as f32 / 100.0;
            let range = range.max(0.01);
            if let Some(index) = self
                .zones
                .iter()
                .position(|zone| zone.manager_channel == channel)
            {
                self.zones[index].manager_pitch_range = range;
                self.apply_zone_pitch_ranges();
                return vec![MpeDecoderEvent::ConfigurationChanged(
                    MpeConfiguration::from_zone(self.zones[index]),
                )];
            } else if let Some(index) = self
                .zones
                .iter()
                .position(|zone| zone.contains_member(channel))
            {
                self.zones[index].member_pitch_range = range;
                self.apply_zone_pitch_ranges();
                return vec![MpeDecoderEvent::ConfigurationChanged(
                    MpeConfiguration::from_zone(self.zones[index]),
                )];
            } else {
                self.channels[channel as usize].pitch_config.range_semitones = range;
            }
            return Vec::new();
        }
        if rpn.msb == 0 && rpn.lsb == 6 && !lsb {
            let count = data_entry_msb.clamp(1, 15);
            if let Some(index) = self
                .zones
                .iter()
                .position(|zone| zone.manager_channel == channel)
            {
                let zone = self.zones[index].with_member_channels(count);
                self.zones[index] = zone;
                self.apply_zone_pitch_ranges();
                return vec![MpeDecoderEvent::ConfigurationChanged(
                    MpeConfiguration::from_zone(zone),
                )];
            }
        }
        Vec::new()
    }

    fn update_poly_pressure(
        &mut self,
        channel: u8,
        note: u8,
        value: u8,
        position: Tick,
    ) -> Vec<MpeDecoderEvent> {
        let channel = channel.min(15);
        let value = value as f32 / 127.0;
        let Some(active) = self.active_notes.get_mut(&channel) else {
            return Vec::new();
        };
        if active.pitch != note.min(127) {
            return Vec::new();
        }
        let point = ExpressionPoint {
            position: (position - active.start).max(0.0),
            value,
            interpolation: ExpressionInterpolation::Linear,
        };
        active.expression.pressure.push_raw(point);
        vec![MpeDecoderEvent::Expression {
            note_id: active.note_id,
            lane: NoteExpressionLane::Pressure,
            point,
        }]
    }

    fn update_expression(
        &mut self,
        channel: u8,
        lane: NoteExpressionLane,
        value: f32,
        position: Tick,
    ) -> Vec<MpeDecoderEvent> {
        let channel = channel.min(15);
        let value = match lane {
            NoteExpressionLane::Pitch => value.clamp(-1.0, 1.0),
            NoteExpressionLane::Pressure | NoteExpressionLane::Timbre => value.clamp(0.0, 1.0),
        };
        match lane {
            NoteExpressionLane::Pitch => self.channels[channel as usize].pitch_bend = value,
            NoteExpressionLane::Pressure => self.channels[channel as usize].pressure = value,
            NoteExpressionLane::Timbre => self.channels[channel as usize].timbre = value,
        }
        // A controller arriving on an MPE manager channel is global to the
        // zone. Fan it out to every currently sounding member note so a
        // global pitch wheel/pressure/timbre gesture is not silently lost.
        if let Some(zone) = self
            .zones
            .iter()
            .find(|zone| zone.manager_channel == channel)
            .copied()
        {
            let mut events = Vec::new();
            for member_channel in zone.member_channels() {
                let Some(active) = self.active_notes.get_mut(&member_channel) else {
                    continue;
                };
                let point = ExpressionPoint {
                    position: (position - active.start).max(0.0),
                    value,
                    interpolation: ExpressionInterpolation::Linear,
                };
                match lane {
                    NoteExpressionLane::Pitch => active.expression.pitch.push_raw(point),
                    NoteExpressionLane::Pressure => active.expression.pressure.push_raw(point),
                    NoteExpressionLane::Timbre => active.expression.timbre.push_raw(point),
                }
                events.push(MpeDecoderEvent::Expression {
                    note_id: active.note_id,
                    lane,
                    point,
                });
            }
            return events;
        }
        match self.active_notes.get_mut(&channel) {
            Some(active) => {
                let point = ExpressionPoint {
                    position: (position - active.start).max(0.0),
                    value,
                    interpolation: ExpressionInterpolation::Linear,
                };
                match lane {
                    NoteExpressionLane::Pitch => active.expression.pitch.push_raw(point),
                    NoteExpressionLane::Pressure => active.expression.pressure.push_raw(point),
                    NoteExpressionLane::Timbre => active.expression.timbre.push_raw(point),
                }
                vec![MpeDecoderEvent::Expression {
                    note_id: active.note_id,
                    lane,
                    point,
                }]
            }
            None => Vec::new(),
        }
    }

    fn release_sustained(&mut self, channel: u8, position: Tick) -> Vec<MpeDecoderEvent> {
        let Some(active) = self.active_notes.get(&channel) else {
            return Vec::new();
        };
        if active.key_down {
            return Vec::new();
        }
        let active = self.active_notes.remove(&channel).expect("checked above");
        self.channels[channel as usize].active_note = None;
        vec![note_off_event(active, position, None)]
    }

    /// MPE sends global pedals on the zone's manager channel. A pedal on a
    /// member channel remains local to that member, which also keeps the
    /// decoder useful for controllers that send ordinary per-channel MIDI.
    fn sustain_channels(&self, channel: u8) -> Vec<u8> {
        let channel = channel.min(15);
        let mut channels = vec![channel];
        for zone in &self.zones {
            if zone.manager_channel == channel {
                channels.extend(zone.member_channels());
            }
        }
        channels.sort_unstable();
        channels.dedup();
        channels
    }

    fn finish_all(&mut self, position: Tick) -> Vec<MpeDecoderEvent> {
        let mut channels: Vec<u8> = self.active_notes.keys().copied().collect();
        channels.sort_unstable();
        let mut events = Vec::with_capacity(channels.len());
        for channel in channels {
            if let Some(active) = self.active_notes.remove(&channel) {
                self.channels[channel as usize].active_note = None;
                events.push(note_off_event(active, position, None));
            }
        }
        events
    }

    fn apply_zone_pitch_ranges(&mut self) {
        for state in &mut self.channels {
            state.pitch_config = crate::expression::PitchExpressionConfig::default();
        }
        for zone in &self.zones {
            for channel in zone.member_channels() {
                self.channels[channel as usize].pitch_config =
                    crate::expression::PitchExpressionConfig::new(zone.member_pitch_range);
            }
            self.channels[zone.manager_channel as usize].pitch_config =
                crate::expression::PitchExpressionConfig::new(zone.manager_pitch_range);
        }
    }
}

fn bend_to_normalized(value: u16) -> f32 {
    ((value.min(16_383) as f32 - 8_192.0) / 8_192.0).clamp(-1.0, 1.0)
}

fn curve_with_initial(value: f32) -> crate::expression::ExpressionCurve {
    crate::expression::ExpressionCurve::from_points(vec![ExpressionPoint {
        position: 0.0,
        value,
        interpolation: ExpressionInterpolation::Linear,
    }])
}

fn note_off_event(
    note: ActiveMpeNote,
    position: Tick,
    release_velocity: Option<f32>,
) -> MpeDecoderEvent {
    MpeDecoderEvent::NoteOff {
        note_id: note.note_id,
        channel: note.channel,
        pitch: note.pitch,
        position,
        release_velocity,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::expression::NoteExpressionLane;

    fn decoder() -> MpeDecoder {
        MpeDecoder::new(vec![MpeZone::lower(15), MpeZone::upper(15)])
    }

    #[test]
    fn lower_zone_keeps_expression_on_note_id_not_channel() {
        let mut decoder = decoder();
        let first = decoder.process(
            MpeMidiMessage::NoteOn {
                channel: 1,
                note: 60,
                velocity: 110,
            },
            0.0,
        );
        let note_id = match &first[0] {
            MpeDecoderEvent::NoteOn { note_id, .. } => *note_id,
            _ => unreachable!(),
        };
        let expression = decoder.process(
            MpeMidiMessage::PitchBend {
                channel: 1,
                value: 10_240,
            },
            0.25,
        );
        assert!(matches!(
            expression.as_slice(),
            [MpeDecoderEvent::Expression {
                note_id: id,
                lane: NoteExpressionLane::Pitch,
                ..
            }] if *id == note_id
        ));
        let second = decoder.process(
            MpeMidiMessage::NoteOn {
                channel: 2,
                note: 64,
                velocity: 100,
            },
            0.3,
        );
        assert!(matches!(second[0], MpeDecoderEvent::NoteOn { note_id: id, .. } if id != note_id));
    }

    #[test]
    fn pressure_and_cc74_are_decoded_to_their_active_note() {
        let mut decoder = decoder();
        let events = decoder.process(
            MpeMidiMessage::NoteOn {
                channel: 1,
                note: 60,
                velocity: 100,
            },
            1.0,
        );
        let note_id = match events[0] {
            MpeDecoderEvent::NoteOn { note_id, .. } => note_id,
            _ => unreachable!(),
        };
        decoder.process(
            MpeMidiMessage::ChannelPressure {
                channel: 1,
                value: 80,
            },
            1.2,
        );
        let events = decoder.process(
            MpeMidiMessage::ControlChange {
                channel: 1,
                controller: 74,
                value: 90,
            },
            1.3,
        );
        assert!(
            matches!(events[0], MpeDecoderEvent::Expression { note_id: id, lane: NoteExpressionLane::Timbre, .. } if id == note_id)
        );
    }

    #[test]
    fn poly_pressure_targets_the_matching_note_when_present() {
        let mut decoder = decoder();
        let note_id = match decoder.process(
            MpeMidiMessage::NoteOn {
                channel: 1,
                note: 60,
                velocity: 100,
            },
            0.0,
        )[0]
        {
            MpeDecoderEvent::NoteOn { note_id, .. } => note_id,
            _ => unreachable!(),
        };
        let events = decoder.process(
            MpeMidiMessage::PolyPressure {
                channel: 1,
                note: 60,
                value: 96,
            },
            0.5,
        );
        assert!(matches!(
            events.as_slice(),
            [MpeDecoderEvent::Expression {
                note_id: id,
                lane: NoteExpressionLane::Pressure,
                ..
            }] if *id == note_id
        ));
        assert!(
            decoder
                .process(
                    MpeMidiMessage::PolyPressure {
                        channel: 1,
                        note: 61,
                        value: 96,
                    },
                    0.6,
                )
                .is_empty()
        );
    }

    #[test]
    fn pitch_bend_before_note_on_is_captured_as_initial_expression() {
        let mut decoder = decoder();
        decoder.process(
            MpeMidiMessage::PitchBend {
                channel: 1,
                value: 12_288,
            },
            0.0,
        );
        let events = decoder.process(
            MpeMidiMessage::NoteOn {
                channel: 1,
                note: 60,
                velocity: 100,
            },
            1.0,
        );
        let MpeDecoderEvent::NoteOn { expression, .. } = &events[0] else {
            unreachable!()
        };
        assert!((expression.pitch.points[0].value - 0.5).abs() < 0.001);
    }

    #[test]
    fn sustain_delays_note_off_and_channel_reuse_until_pedal_up() {
        let mut decoder = decoder();
        decoder.process(
            MpeMidiMessage::ControlChange {
                channel: 1,
                controller: 64,
                value: 127,
            },
            0.0,
        );
        decoder.process(
            MpeMidiMessage::NoteOn {
                channel: 1,
                note: 60,
                velocity: 100,
            },
            0.0,
        );
        assert!(
            decoder
                .process(
                    MpeMidiMessage::NoteOff {
                        channel: 1,
                        note: 60,
                        release_velocity: 0,
                    },
                    1.0,
                )
                .is_empty()
        );
        assert!(decoder.active_notes().contains_key(&1));
        let events = decoder.process(
            MpeMidiMessage::ControlChange {
                channel: 1,
                controller: 64,
                value: 0,
            },
            2.0,
        );
        assert!(matches!(events[0], MpeDecoderEvent::NoteOff { .. }));
        assert!(decoder.active_notes().is_empty());
    }

    #[test]
    fn manager_sustain_applies_to_all_lower_zone_members() {
        let mut decoder = MpeDecoder::default();
        let note_id = match decoder.process(
            MpeMidiMessage::NoteOn {
                channel: 3,
                note: 60,
                velocity: 100,
            },
            0.0,
        )[0]
        {
            MpeDecoderEvent::NoteOn { note_id, .. } => note_id,
            _ => unreachable!(),
        };
        decoder.process(
            MpeMidiMessage::ControlChange {
                channel: 0,
                controller: 64,
                value: 127,
            },
            0.1,
        );
        assert!(
            decoder
                .process(
                    MpeMidiMessage::NoteOff {
                        channel: 3,
                        note: 60,
                        release_velocity: 0,
                    },
                    1.0,
                )
                .is_empty()
        );
        assert_eq!(decoder.active_notes().get(&3).unwrap().note_id, note_id);
        let events = decoder.process(
            MpeMidiMessage::ControlChange {
                channel: 0,
                controller: 64,
                value: 0,
            },
            2.0,
        );
        assert!(matches!(
            events.as_slice(),
            [MpeDecoderEvent::NoteOff { note_id: id, .. }] if *id == note_id
        ));
    }

    #[test]
    fn manager_rpn_six_changes_zone_member_count() {
        let mut decoder = MpeDecoder::default();
        decoder.process(
            MpeMidiMessage::ControlChange {
                channel: 0,
                controller: 101,
                value: 0,
            },
            0.0,
        );
        decoder.process(
            MpeMidiMessage::ControlChange {
                channel: 0,
                controller: 100,
                value: 6,
            },
            0.0,
        );
        let events = decoder.process(
            MpeMidiMessage::ControlChange {
                channel: 0,
                controller: 6,
                value: 4,
            },
            0.0,
        );
        assert!(matches!(
            events[0],
            MpeDecoderEvent::ConfigurationChanged(MpeConfiguration {
                member_channels: 4,
                ..
            })
        ));
        assert_eq!(decoder.zones()[0].member_channels().len(), 4);
    }

    #[test]
    fn manager_rpn_zero_updates_manager_pitch_range() {
        let mut decoder = MpeDecoder::new(vec![MpeZone::lower(3)]);
        for message in [
            MpeMidiMessage::ControlChange {
                channel: 0,
                controller: 101,
                value: 0,
            },
            MpeMidiMessage::ControlChange {
                channel: 0,
                controller: 100,
                value: 0,
            },
            MpeMidiMessage::ControlChange {
                channel: 0,
                controller: 6,
                value: 24,
            },
        ] {
            decoder.process(message, 0.0);
        }
        assert_eq!(decoder.pitch_range_for(1), 48.0);
        assert_eq!(decoder.pitch_range_for(0), 24.0);
    }

    #[test]
    fn member_rpn_zero_updates_member_pitch_range() {
        let mut decoder = MpeDecoder::new(vec![MpeZone::lower(3)]);
        for message in [
            MpeMidiMessage::ControlChange {
                channel: 1,
                controller: 101,
                value: 0,
            },
            MpeMidiMessage::ControlChange {
                channel: 1,
                controller: 100,
                value: 0,
            },
            MpeMidiMessage::ControlChange {
                channel: 1,
                controller: 6,
                value: 24,
            },
        ] {
            decoder.process(message, 0.0);
        }
        assert_eq!(decoder.pitch_range_for(1), 24.0);
        assert_eq!(decoder.pitch_range_for(0), 2.0);
    }

    #[test]
    fn manager_expression_is_applied_to_all_active_member_notes() {
        let mut decoder = MpeDecoder::default();
        let first = match decoder.process(
            MpeMidiMessage::NoteOn {
                channel: 1,
                note: 60,
                velocity: 100,
            },
            0.0,
        )[0]
        {
            MpeDecoderEvent::NoteOn { note_id, .. } => note_id,
            _ => unreachable!(),
        };
        let second = match decoder.process(
            MpeMidiMessage::NoteOn {
                channel: 2,
                note: 64,
                velocity: 100,
            },
            0.0,
        )[0]
        {
            MpeDecoderEvent::NoteOn { note_id, .. } => note_id,
            _ => unreachable!(),
        };
        let events = decoder.process(
            MpeMidiMessage::PitchBend {
                channel: 0,
                value: 12_288,
            },
            0.5,
        );
        assert_eq!(events.len(), 2);
        assert!(events.iter().all(|event| matches!(
            event,
            MpeDecoderEvent::Expression {
                lane: NoteExpressionLane::Pitch,
                ..
            }
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            MpeDecoderEvent::Expression { note_id, .. } if *note_id == first
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            MpeDecoderEvent::Expression { note_id, .. } if *note_id == second
        )));
    }

    #[test]
    fn zero_velocity_note_on_is_treated_as_note_off() {
        let mut decoder = MpeDecoder::default();
        decoder.process(
            MpeMidiMessage::NoteOn {
                channel: 1,
                note: 60,
                velocity: 100,
            },
            0.0,
        );
        let events = decoder.process(
            MpeMidiMessage::NoteOn {
                channel: 1,
                note: 60,
                velocity: 0,
            },
            1.0,
        );
        assert!(matches!(
            events.as_slice(),
            [MpeDecoderEvent::NoteOff { .. }]
        ));
    }

    #[test]
    fn all_sound_off_terminates_active_notes() {
        let mut decoder = MpeDecoder::default();
        decoder.process(
            MpeMidiMessage::NoteOn {
                channel: 1,
                note: 60,
                velocity: 100,
            },
            0.0,
        );
        let events = decoder.process(
            MpeMidiMessage::ControlChange {
                channel: 0,
                controller: 120,
                value: 0,
            },
            1.0,
        );
        assert!(matches!(
            events.as_slice(),
            [MpeDecoderEvent::NoteOff { .. }]
        ));
        assert!(decoder.active_notes().is_empty());
    }
}
