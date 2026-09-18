//! Floating audio-editor tool windows.

mod preview;
mod viz;
mod window;
mod workspace;

pub use preview::{ClipPreviewOverride, apply_preview_to_clip, apply_previews_to_snapshot};
pub use window::{
    AUDIO_TOOL_WINDOW_MIN_HEIGHT, AUDIO_TOOL_WINDOW_MIN_WIDTH, AudioToolCommand, AudioToolWindow,
    AudioToolWindowCallbacks, AudioToolWindowManager, open_audio_tool_window,
};
