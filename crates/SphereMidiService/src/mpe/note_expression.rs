use super::{MpeDecoder, MpeDecoderEvent};
use crate::MidiInputEvent;
use crate::expression::{ExpressionSimplificationTolerances, NoteExpression, NoteId, Tick};
use std::collections::HashMap;

/// A completed note produced by the MPE recording path.  The source channel
/// is intentionally not present: it is transport metadata and is discarded at
/// the decoder boundary.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordedNote {
    pub id: NoteId,
    pub pitch: u8,
    pub velocity: u8,
    pub start: Tick,
    pub duration: Tick,
    pub release_velocity: Option<f32>,
    pub expression: NoteExpression,
}

#[derive(Debug, Clone)]
struct RecordingNote {
    id: NoteId,
    pitch: u8,
    velocity: u8,
    start: Tick,
    expression: NoteExpression,
}

/// Control-thread recorder for MPE input.  It stores every incoming point
/// while recording and simplifies only in [`Self::finish`].
#[derive(Debug, Clone)]
pub struct MpeRecordingSession {
    decoder: MpeDecoder,
    notes: HashMap<NoteId, RecordingNote>,
    completed: Vec<RecordedNote>,
    tolerances: ExpressionSimplificationTolerances,
}

impl MpeRecordingSession {
    pub fn new(decoder: MpeDecoder) -> Self {
        let pitch_range = decoder
            .zones()
            .first()
            .map(|zone| zone.member_pitch_range)
            .unwrap_or(48.0);
        Self {
            decoder,
            notes: HashMap::new(),
            completed: Vec::new(),
            tolerances: ExpressionSimplificationTolerances::for_pitch_range(pitch_range),
        }
    }

    pub fn decoder(&self) -> &MpeDecoder {
        &self.decoder
    }

    pub fn decoder_mut(&mut self) -> &mut MpeDecoder {
        &mut self.decoder
    }

    pub fn set_tolerances(&mut self, tolerances: ExpressionSimplificationTolerances) {
        self.tolerances = tolerances;
    }

    pub fn process(&mut self, event: &MidiInputEvent, position: Tick) {
        let events = self.decoder.process_input(event, position);
        self.consume(events);
    }

    /// Finish the take at the supplied note-local position.  Any held or
    /// sustain-held notes are closed here, then curves are optimized off the
    /// input path.
    pub fn finish(mut self, end_position: Tick) -> Vec<RecordedNote> {
        let final_events = self.decoder.finish(end_position);
        self.consume(final_events);
        for note in self.notes.into_values() {
            self.completed.push(RecordedNote {
                id: note.id,
                pitch: note.pitch,
                velocity: note.velocity,
                start: note.start,
                duration: (end_position - note.start).max(0.0),
                release_velocity: note.expression.release_velocity,
                expression: note.expression,
            });
        }
        for note in &mut self.completed {
            note.expression = note.expression.simplify(self.tolerances);
        }
        self.completed
            .sort_by(|a, b| a.start.total_cmp(&b.start).then_with(|| a.id.cmp(&b.id)));
        self.completed
    }

    fn consume(&mut self, events: Vec<MpeDecoderEvent>) {
        for event in events {
            match event {
                MpeDecoderEvent::NoteOn {
                    note_id,
                    pitch,
                    velocity,
                    position,
                    expression,
                    ..
                } => {
                    self.notes.insert(
                        note_id,
                        RecordingNote {
                            id: note_id,
                            pitch,
                            velocity,
                            start: position,
                            expression,
                        },
                    );
                }
                MpeDecoderEvent::Expression {
                    note_id,
                    lane,
                    point,
                } => {
                    if let Some(note) = self.notes.get_mut(&note_id) {
                        match lane {
                            crate::expression::NoteExpressionLane::Pitch => {
                                note.expression.pitch.push_raw(point)
                            }
                            crate::expression::NoteExpressionLane::Pressure => {
                                note.expression.pressure.push_raw(point)
                            }
                            crate::expression::NoteExpressionLane::Timbre => {
                                note.expression.timbre.push_raw(point)
                            }
                        }
                    }
                }
                MpeDecoderEvent::NoteOff {
                    note_id,
                    position,
                    release_velocity,
                    ..
                } => {
                    if let Some(mut note) = self.notes.remove(&note_id) {
                        note.expression.release_velocity = release_velocity
                            .filter(|value| *value > 0.0)
                            .or(note.expression.release_velocity);
                        self.completed.push(RecordedNote {
                            id: note.id,
                            pitch: note.pitch,
                            velocity: note.velocity,
                            start: note.start,
                            duration: (position - note.start).max(0.0),
                            release_velocity: note.expression.release_velocity,
                            expression: note.expression,
                        });
                    }
                }
                MpeDecoderEvent::ConfigurationChanged(_) => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recording_keeps_raw_note_expression_until_finish() {
        let mut recorder = MpeRecordingSession::new(MpeDecoder::default());
        recorder.process(
            &MidiInputEvent::NoteOn {
                note: 60,
                velocity: 110,
                channel: 1,
            },
            1.0,
        );
        recorder.process(
            &MidiInputEvent::PitchBend {
                value: 12_288,
                channel: 1,
            },
            1.25,
        );
        recorder.process(
            &MidiInputEvent::PitchBend {
                value: 10_240,
                channel: 1,
            },
            1.75,
        );
        recorder.process(
            &MidiInputEvent::ChannelPressure {
                value: 90,
                channel: 1,
            },
            1.5,
        );
        recorder.process(
            &MidiInputEvent::ChannelPressure {
                value: 64,
                channel: 1,
            },
            1.75,
        );
        recorder.process(
            &MidiInputEvent::NoteOff {
                note: 60,
                channel: 1,
            },
            2.0,
        );
        let notes = recorder.finish(2.0);
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].start, 1.0);
        assert_eq!(notes[0].duration, 1.0);
        assert!(notes[0].expression.pitch.points.len() >= 2);
        assert!(notes[0].expression.pressure.points.len() >= 2);
    }
}
