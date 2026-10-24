//! Opt-in Windows GPU video transport. Audio remains owned by PlaybackSession.
#[allow(dead_code)] // Also included by isolated proofs with different API subsets.
pub(crate) mod convert;
#[allow(dead_code)]
pub(crate) mod decode;
pub use convert::SharedOutput as GpuPlaybackOutput;
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
};

#[derive(Clone)]
pub struct GpuPlaybackConfig {
    /// The actual presentation renderer's adapter; never guessed from GPU vendor.
    pub adapter_luid: (u32, i32),
    /// Absolute preferred runtime directory; empty selects bounded Automatic
    /// discovery on the worker. Existing explicit-directory callers remain valid.
    pub dll_directory: PathBuf,
    /// Nonblocking, coalescing UI notification. Called without internal locks.
    pub wake: Option<Arc<dyn Fn() + Send + Sync>>,
}
impl GpuPlaybackConfig {
    /// Enables bounded runtime discovery without shell environment configuration.
    /// This does not change PlaybackSession's default CPU preview policy.
    pub fn automatic(adapter_luid: (u32, i32)) -> Self {
        Self {
            adapter_luid,
            dll_directory: PathBuf::new(),
            wake: None,
        }
    }
    pub fn runtime_config(&self) -> crate::ffmpeg_runtime::FfmpegRuntimeConfig {
        if self.dll_directory.as_os_str().is_empty() {
            crate::ffmpeg_runtime::FfmpegRuntimeConfig::Automatic
        } else {
            crate::ffmpeg_runtime::FfmpegRuntimeConfig::Directory(self.dll_directory.clone())
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GpuPlaybackStatus {
    Starting,
    Active,
    Fallback(String),
    Failed(String),
    Finished,
}
#[derive(Clone, Copy, Debug, Default)]
pub struct GpuPlaybackStats {
    pub decoded: u64,
    pub published: u64,
    pub dropped: u64,
    pub max_future_frames: u64,
    pub max_inflight_reads: u64,
    pub last_published_id: u64,
    pub last_publish_position: u64,
    pub pool_size: u64,
    pub seek_target_ns: u64,
    pub first_source_ns: Option<u64>,
    pub first_published_id: u64,
    pub preroll_frames: u64,
    pub busy_skips: u64,
    pub queue_full_skips: u64,
    pub busy_wait_ns: u64,
    pub busy_release_notifications: u64,
    pub queue_full_wait_ns: u64,
    pub clock_wait_ns: u64,
}
pub(crate) struct GpuState {
    pub cancel: Arc<AtomicBool>,
    pub status: Mutex<GpuPlaybackStatus>,
    pub output: Mutex<Option<GpuPlaybackOutput>>,
    pub stats: Mutex<GpuPlaybackStats>,
    pub config: GpuPlaybackConfig,
    pub disabled: AtomicBool,
    pub publication_epoch: AtomicU64,
}
impl GpuState {
    pub fn new(config: GpuPlaybackConfig) -> Self {
        Self {
            cancel: Arc::new(AtomicBool::new(false)),
            status: Mutex::new(GpuPlaybackStatus::Starting),
            output: Mutex::new(None),
            stats: Mutex::new(GpuPlaybackStats::default()),
            config,
            disabled: AtomicBool::new(false),
            publication_epoch: AtomicU64::new(0),
        }
    }
    pub fn notify(&self) {
        if let Some(wake) = &self.config.wake {
            wake();
        }
    }
    pub fn status(&self, status: GpuPlaybackStatus) {
        *self.status.lock().unwrap() = status;
        self.notify();
    }
    pub fn disable(&self) {
        self.disabled.store(true, Ordering::Release);
        self.cancel.store(true, Ordering::Release);
    }
}
