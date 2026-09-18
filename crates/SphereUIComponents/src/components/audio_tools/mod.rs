//! Floating audio-editor tool windows.

mod preview;
mod window;

pub use preview::{apply_preview_to_clip, apply_previews_to_snapshot, ClipPreviewOverride};
pub use window::{
    open_audio_tool_window, AudioToolCommand, AudioToolWindow, AudioToolWindowCallbacks,
    AudioToolWindowManager, AUDIO_TOOL_WINDOW_MIN_HEIGHT, AUDIO_TOOL_WINDOW_MIN_WIDTH,
};
