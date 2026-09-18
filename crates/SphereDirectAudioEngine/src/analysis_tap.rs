//! Preallocated analysis tap: audio callback copies stereo frames, workers FFT.
//!
//! The ring is process-wide so the render callback never looks up a HashMap.
//! Control thread sets the target clip hash; the callback writes only when the
//! playing clip matches.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

use crate::input_ring::InputRing;

/// FNV-1a of a clip id. Never returns 0 (0 means "tap disabled").
pub fn clip_id_hash(id: &str) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in id.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    if hash == 0 {
        1
    } else {
        hash
    }
}

pub struct AnalysisTap {
    pub ring: InputRing,
    target_hash: AtomicU64,
}

impl Default for AnalysisTap {
    fn default() -> Self {
        Self {
            ring: InputRing::default(),
            target_hash: AtomicU64::new(0),
        }
    }
}

impl AnalysisTap {
    pub fn set_target_clip(&self, clip_id: Option<&str>) {
        let hash = clip_id.map(clip_id_hash).unwrap_or(0);
        self.target_hash.store(hash, Ordering::Release);
        self.ring.set_active(hash != 0, 2, 0);
    }

    pub fn target_hash(&self) -> u64 {
        self.target_hash.load(Ordering::Acquire)
    }

    #[inline]
    pub fn write_if_target(&self, clip_hash: u64, left: f32, right: f32) {
        if clip_hash != 0 && clip_hash == self.target_hash.load(Ordering::Relaxed) {
            self.ring.write_stereo(left, right);
        }
    }

    pub fn copy_recent(&self, left: &mut [f32], right: &mut [f32]) -> usize {
        let n = left.len().min(right.len());
        if n == 0 {
            return 0;
        }
        let head = self.ring.write_head();
        if head < n as u64 {
            return 0;
        }
        let start = head - n as u64;
        for i in 0..n {
            let (l, r) = self.ring.read_frame(start + i as u64);
            left[i] = l;
            right[i] = r;
        }
        n
    }
}

pub fn analysis_tap() -> &'static AnalysisTap {
    static TAP: OnceLock<AnalysisTap> = OnceLock::new();
    TAP.get_or_init(AnalysisTap::default)
}
