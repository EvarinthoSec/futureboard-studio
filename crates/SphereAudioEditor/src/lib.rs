//! Audio clip editor for Futureboard Studio — waveform view without PCM buffers.

mod audio_editor;
mod audio_editor_state;
mod audio_ruler;
mod editing;
mod editor_kind;
mod spectrogram;
mod tools;
mod viewport;
mod waveform_view;

pub use audio_editor::{
    AUDIO_EDITOR_INSPECTOR_WIDTH, AUDIO_EDITOR_TOOLS_WIDTH, AudioEditorCallbacks,
    AudioEditorCanvasDrag, AudioEditorDropdown, AudioEditorEvent, AudioEditorTheme,
    AudioEditorViewModel, audio_editor_panel, default_wheel_handler, default_wheel_handler_at,
    empty_audio_editor,
};
pub use audio_editor_state::AudioEditorState;
pub use editing::{
    AudioChannelMode, AudioEditorDrag, AudioEditorSnap, AudioEditorTool, AudioFadeCurve,
    AudioFadeEdge, AudioRangeSelection, ClipEnvelope, EnvelopeCurve, EnvelopePoint,
    SpectralSelection,
};
pub use editor_kind::{ClipEditorKind, ClipTypeHint, editor_kind_for_clip};
pub use spectrogram::{
    AmplitudeScale, AudioEditorViewMode, FrequencyScale, SpectrogramAnalysis,
    SpectrogramAnalysisError, SpectrogramSettings, SpectrogramTile, SpectrogramTileView,
    SpectrogramViewModel, analyze_spectrogram,
};
pub use tools::{AudioRepairModule, AudioToolKind, AudioToolSession, AudioToolTarget};
pub use viewport::AudioEditorViewport;
pub use waveform_view::{WaveformColumn, WaveformViewModel};
