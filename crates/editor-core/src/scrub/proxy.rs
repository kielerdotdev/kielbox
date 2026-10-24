//! Reusable intra-frame seek copies; original projects and exports remain untouched.
use super::native::Decoder;
use crate::{media_command, Content, PreviewFrame, Project, TrackKind};
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStderr, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

// Quality is fixed independently of storage: oversized copies fall back to the source.
const FILE_BUDGET: u64 = 2 * 1024 * 1024 * 1024;
const RESERVATION: u64 = FILE_BUDGET + 16 * 1024 * 1024;
const MIN_FREE_BYTES: u64 = RESERVATION + 512 * 1024 * 1024;
const CACHE_BUDGET: u64 = 3 * 1024 * 1024 * 1024;
const RECIPE: u32 = 7;
const STDERR_TAIL_BYTES: usize = 16 * 1024;
const ENCODER_ARGS: &[&str] = &[
    "-c:v", "h264_nvenc", "-preset", "p3", "-tune", "ull", "-g", "1", "-bf", "0", "-rc",
    "constqp", "-qp", "24", "-i_qfactor", "1", "-i_qoffset", "0", "-b:v", "0",
    "-rc-lookahead", "0", "-multipass", "disabled",
    "-spatial-aq", "0", "-temporal-aq", "0", "-slices", "2",
];
fn proxy_filter(source_size: (u32, u32)) -> String {
    format!(
        "scale_cuda={}:{}:force_original_aspect_ratio=decrease:force_divisible_by=2:interp_algo=bicubic,setpts=PTS-STARTPTS",
        source_size.0.min(1920),
        source_size.1.min(1080),
    )
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Identity {
    path: Vec<u16>,
    bytes: u64,
    modified_ns: u128,
    stream: u32,
    recipe: u32,
}
impl Identity {
    fn read(path: &Path, stream: u32) -> Result<Self> {
        use std::os::windows::ffi::OsStrExt;
        let path = path.canonicalize()?;
        let metadata = path.metadata()?;
        ensure!(
            metadata.is_file() && metadata.len() > 0,
            "Empty seek source"
        );
        Ok(Self {
            path: path.as_os_str().encode_wide().collect(),
            bytes: metadata.len(),
            modified_ns: metadata.modified()?.duration_since(UNIX_EPOCH)?.as_nanos(),
            stream,
            recipe: RECIPE,
        })
    }
    fn key(&self) -> Result<String> {
        // Stable FNV-1a. Complete identity is verified too: collisions cannot alias sources.
        let mut hash = 0x6c62272e07bb014262b821756295c58du128;
        for byte in serde_json::to_vec(self)? {
            hash = (hash ^ byte as u128).wrapping_mul(0x0000000001000000000000000000013b);
        }
        Ok(format!("{hash:032x}"))
    }
}
#[derive(Serialize, Deserialize)]
struct Complete {
    identity: Identity,
    bytes: u64,
}
struct Cache {
    root: PathBuf,
    key: String,
}
impl Cache {
    fn new(identity: &Identity) -> Result<Self> {
        let root = std::env::var_os("RCR_SEEK_CACHE_DIR").map_or_else(
            || {
                std::env::var_os("LOCALAPPDATA")
                    .map(PathBuf::from)
                    .unwrap_or_else(std::env::temp_dir)
                    .join("rcr/cache/seek-v2")
            },
            PathBuf::from,
        );
        fs::create_dir_all(&root)?;
        Ok(Self {
            root,
            key: identity.key()?,
        })
    }
    fn data(&self) -> PathBuf {
        self.root.join(format!("{}.mkv", self.key))
    }
    fn marker(&self) -> PathBuf {
        self.root.join(format!("{}.json", self.key))
    }
    fn lock(&self, name: &str) -> Result<Option<File>> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.root.join(name))?;
        match file.try_lock() {
            Ok(()) => Ok(Some(file)),
            Err(fs::TryLockError::WouldBlock) => Ok(None),
            Err(fs::TryLockError::Error(error)) => Err(error.into()),
        }
    }
    // Caller holds index lock while checking completion marker and acquiring a read lease.
    fn reuse(&self, identity: &Identity) -> Result<Option<File>> {
        let Ok(marker) = File::open(self.marker()) else {
            return Ok(None);
        };
        let complete: Complete = match serde_json::from_reader(marker.take(256 * 1024)) {
            Ok(complete) => complete,
            Err(_) => return Ok(None),
        };
        if complete.identity != *identity || complete.bytes == 0 || complete.bytes >= FILE_BUDGET {
            return Ok(None);
        }
        let Ok(file) = File::open(self.data()) else {
            return Ok(None);
        };
        if file.metadata()?.len() != complete.bytes {
            return Ok(None);
        }
        file.try_lock_shared()?;
        // Explicit LRU, independent of Windows' often-disabled last-access timestamps.
        if let Ok(marker) = OpenOptions::new().write(true).open(self.marker()) {
            let _ = marker.set_modified(SystemTime::now());
        }
        Ok(Some(file))
    }
    // Requires both index and encoder locks: no other process can be generating a partial.
    fn make_room(&self, reserve: u64) -> Result<()> {
        self.make_room_with_budget(reserve, CACHE_BUDGET)
    }
    fn make_room_with_budget(&self, reserve: u64, budget: u64) -> Result<()> {
        let mut entries = Vec::new();
        let mut total = 0u64;
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            let path = entry.path();
            if !entry.file_type()?.is_file() {
                continue;
            }
            if entry
                .file_name()
                .to_string_lossy()
                .starts_with("rcr-partial-")
            {
                if fs::remove_file(&path).is_err() {
                    total = total.saturating_add(entry.metadata()?.len());
                }
                continue;
            }
            // Only app-generated hash entries belong to this cache. Never delete
            // unrelated files if a user points the cache override at a shared folder.
            if !cache_entry(&path) {
                continue;
            }
            if path
                .extension()
                .is_some_and(|extension| extension == "json")
            {
                if !path.with_extension("mkv").exists() && fs::remove_file(&path).is_err() {
                    total = total.saturating_add(entry.metadata()?.len());
                }
                continue;
            }
            if path.extension().is_none_or(|extension| extension != "mkv") {
                continue;
            }
            let metadata = entry.metadata()?;
            let marker = path.with_extension("json");
            let marker_metadata = marker.metadata().ok();
            let touched = marker_metadata
                .as_ref()
                .and_then(|m| m.modified().ok())
                .unwrap_or(UNIX_EPOCH);
            let bytes = metadata
                .len()
                .saturating_add(marker_metadata.map_or(0, |m| m.len()));
            total = total.saturating_add(bytes);
            entries.push((touched, path, bytes, marker.is_file()));
        }
        entries.sort_by_key(|entry| entry.0);
        for (_, path, bytes, complete) in entries {
            if complete && total.saturating_add(reserve) <= budget {
                continue;
            }
            if evict(&path) {
                total = total.saturating_sub(bytes);
            }
        }
        ensure!(
            total.saturating_add(reserve) <= budget,
            "Seek cache is occupied by active previews"
        );
        Ok(())
    }
}
fn cache_entry(path: &Path) -> bool {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .is_some_and(|stem| stem.len() == 32 && stem.bytes().all(|byte| byte.is_ascii_hexdigit()))
}
fn evict(path: &Path) -> bool {
    let Ok(file) = OpenOptions::new().read(true).write(true).open(path) else {
        return false;
    };
    if file.try_lock().is_err() {
        return false;
    }
    // Delete-shared Windows handle; index lock prevents lookup racing eviction.
    if fs::remove_file(path).is_err() {
        return false;
    }
    let _ = fs::remove_file(path.with_extension("json"));
    true
}
struct Preparation {
    child: Child,
    job: Option<ProcessJob>,
    _encoder: Option<File>,
    _traced_encoder: Option<Arc<File>>,
    output: tempfile::TempPath,
    started: Instant,
    last_trace: Instant,
    last_bytes: u64,
    trace_key: Option<String>,
    stderr: Option<JoinHandle<()>>,
    stderr_tail: Option<Arc<Mutex<VecDeque<u8>>>>,
}
impl Drop for Preparation {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let status = self.child.wait();
        // Close job-contained descendants before checking the pipe drainer.
        // In normal operation FFmpeg does not create descendants.
        drop(self.job.take());
        let reader_finished = self.stderr.as_ref().is_some_and(|reader| reader.is_finished());
        let reader_detached = self.stderr.is_some() && !(status.is_ok() && reader_finished);
        if let Some(reader) = self.stderr.take() {
            if status.is_ok() && reader_finished {
                let _ = reader.join();
            }
            // Otherwise detach: a wrapper could have created an out-of-job
            // descendant that still owns stderr. Its drainer retains the shared
            // encoder lease until EOF, preventing repeated encoder/reader spawns.
        }
        let tail: Vec<u8> = self
            .stderr_tail
            .as_ref()
            .and_then(|tail| tail.try_lock().ok())
            .map(|tail| tail.iter().copied().collect())
            .unwrap_or_default();
        if let Some(key) = &self.trace_key {
            super::write_seek_trace(
                "Seek proxy trace",
                serde_json::json!({
                    "event": "encoder_reaped", "key": key,
                    "elapsed_ms": self.started.elapsed().as_millis(),
                    "bytes": self.output.metadata().map_or(self.last_bytes, |m| m.len()),
                    "exit_code": status.as_ref().ok().and_then(|status| status.code()),
                    "reaped": status.is_ok(),
                    "stderr_reader_detached": reader_detached,
                    "stderr_tail": String::from_utf8_lossy(&tail),
                    "stderr_tail_limit_bytes": STDERR_TAIL_BYTES,
                })
            );
        }
    }
}
fn read_stderr_tail(mut pipe: ChildStderr, tail: &Mutex<VecDeque<u8>>) {
    let mut chunk = [0u8; 4096];
    loop {
        let count = match pipe.read(&mut chunk) {
            Ok(0) => break,
            Ok(count) => count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        // Never hold the tail mutex while waiting for pipe data.
        if let Ok(mut tail) = tail.lock() {
            retain_tail(&mut tail, &chunk[..count]);
        }
    }
}
fn retain_tail(tail: &mut VecDeque<u8>, bytes: &[u8]) {
    for &byte in bytes {
        if tail.len() == STDERR_TAIL_BYTES {
            tail.pop_front();
        }
        tail.push_back(byte);
    }
}
pub(super) struct Proxy {
    // Declaration order closes decoder before releasing the active-reader file lease.
    decoder: Decoder,
    lease: Option<File>,
    preparation: Option<Preparation>,
    cache: Cache,
    identity: Identity,
    source: PathBuf,
    source_size: (u32, u32),
    asset: u64,
    stream: u32,
    failed: bool,
    started: Instant,
    trace: bool,
}
impl Proxy {
    pub fn start(project: &Project, decoder: &Decoder, cancel: &AtomicBool) -> Result<Self> {
        ensure!(
            !cancel.load(Ordering::Acquire),
            "Seek preparation cancelled"
        );
        ensure!(
            std::env::var_os("RCR_DISABLE_SEEK_PROXY").is_none(),
            "Proxy disabled"
        );
        let videos: Vec<_> = project
            .assets
            .iter()
            .filter(|a| a.streams.iter().any(|s| s.kind == TrackKind::Video))
            .collect();
        ensure!(videos.len() == 1, "Multiple source videos");
        let asset = videos[0];
        ensure!(
            asset.width >= 2 && asset.height >= 2,
            "Invalid seek source dimensions"
        );
        ensure!(
            asset.duration_ns <= 240_000_000_000,
            "Source exceeds proxy budget"
        );
        let stream = asset
            .streams
            .iter()
            .find(|s| s.kind == TrackKind::Video)
            .unwrap()
            .index;
        let identity = Identity::read(&asset.path, stream)?;
        let cache = Cache::new(&identity)?;
        let mut proxy = Self {
            decoder: decoder.software(),
            lease: None,
            preparation: None,
            cache,
            identity,
            source: asset.path.clone(),
            source_size: (asset.width, asset.height),
            asset: asset.id,
            stream,
            failed: false,
            started: Instant::now(),
            trace: std::env::var_os("RCR_TRACE_SEEK").is_some(),
        };
        proxy.poll(cancel);
        Ok(proxy)
    }
    pub fn preparing(&self) -> bool {
        !self.failed && !self.ready()
    }
    pub fn ready(&self) -> bool {
        self.lease.is_some()
    }
    pub fn matches(&self, project: &Project) -> bool {
        project
            .assets
            .iter()
            .filter(|a| a.streams.iter().any(|s| s.kind == TrackKind::Video))
            .count()
            == 1
            && project
                .assets
                .iter()
                .any(|a| a.id == self.asset && a.path == self.source)
    }
    /// Filesystem refresh is deliberately restricted to project revision changes.
    pub fn matches_revision(&self, project: &Project) -> bool {
        self.matches(project)
            && Identity::read(&self.source, self.stream)
                .is_ok_and(|identity| identity == self.identity)
    }
    pub fn cached_surface_bytes(&self) -> usize {
        self.decoder.cached_surface_bytes()
    }
    pub fn surface_hits(&self) -> u64 {
        self.decoder.surface_hits()
    }
    pub fn poll(&mut self, cancel: &AtomicBool) {
        if cancel.load(Ordering::Acquire) {
            if self.trace && self.preparation.is_some() {
                self.trace_event("cancelled", None);
            }
            self.failed = true;
            self.preparation = None;
            return;
        }
        if !self.preparing() {
            return;
        }
        let Err(error) = self.poll_inner(cancel) else {
            return;
        };
        if self.trace {
            self.trace_event(
                "failed",
                Some(format!("{error:#}").chars().take(4096).collect()),
            );
        }
        self.failed = true;
        self.preparation = None;
    }
    fn trace_event(&self, event: &str, error: Option<String>) {
        super::write_seek_trace(
            "Seek proxy trace",
            serde_json::json!({
                "event": event, "key": self.cache.key,
                "elapsed_ms": self.started.elapsed().as_millis(),
                "bytes": self.preparation.as_ref().map(|p| p.output.metadata().map_or(p.last_bytes, |m| m.len())),
                "error": error,
            })
        );
    }
    fn poll_inner(&mut self, cancel: &AtomicBool) -> Result<()> {
        ensure!(
            !cancel.load(Ordering::Acquire),
            "Seek preparation cancelled"
        );
        ensure!(
            self.started.elapsed() < Duration::from_secs(120),
            "Seek preparation expired"
        );
        if let Some(preparation) = &mut self.preparation {
            preparation.last_bytes = preparation.output.metadata().map_or(0, |m| m.len());
            if self.trace && preparation.last_trace.elapsed() >= Duration::from_secs(1) {
                super::write_seek_trace(
                    "Seek proxy trace",
                    serde_json::json!({ "event": "preparing", "key": self.cache.key,
                        "elapsed_ms": preparation.started.elapsed().as_millis(),
                        "bytes": preparation.last_bytes, "file_budget": FILE_BUDGET })
                );
                preparation.last_trace = Instant::now();
            }
            ensure!(
                preparation.started.elapsed() < Duration::from_secs(90),
                "Seek encoder timed out"
            );
            ensure!(
                preparation.last_bytes < FILE_BUDGET,
                "Seek copy too large"
            );
            let Some(status) = preparation.child.try_wait()? else {
                return Ok(());
            };
            ensure!(status.success(), "Seek encoder failed ({status})");
            ensure!(
                Identity::read(&self.source, self.stream)? == self.identity,
                "Source changed during preparation"
            );
            let Some(_index) = self.cache.lock("index.lock")? else {
                return Ok(());
            };
            let mut preparation = self.preparation.take().unwrap();
            let bytes = preparation.output.metadata()?.len();
            preparation.last_bytes = bytes;
            ensure!(bytes > 0 && bytes < FILE_BUDGET, "Invalid seek copy size");
            if self.cache.data().exists() {
                ensure!(evict(&self.cache.data()), "Seek entry is active");
            }
            fs::rename(&preparation.output, self.cache.data())?;
            OpenOptions::new()
                .write(true)
                .open(self.cache.data())?
                .sync_all()?;
            let mut marker = tempfile::Builder::new()
                .prefix("rcr-partial-")
                .tempfile_in(&self.cache.root)?;
            serde_json::to_writer(
                &mut marker,
                &Complete {
                    identity: self.identity.clone(),
                    bytes,
                },
            )?;
            marker.flush()?;
            marker.as_file().sync_all()?;
            // Atomic marker publication is last: incomplete or crashed output is never reusable.
            marker.persist(self.cache.marker())?;
            self.lease = self.cache.reuse(&self.identity)?;
            ensure!(self.ready(), "Seek publication failed");
            if self.trace {
                self.trace_event("published", None);
            }
            return Ok(());
        }
        let Some(_index) = self.cache.lock("index.lock")? else {
            return Ok(());
        };
        if let Some(lease) = self.cache.reuse(&self.identity)? {
            self.lease = Some(lease);
            if self.trace {
                self.trace_event("cache_reused", None);
            }
            if let Some(_encoder) = self.cache.lock("encoder.lock")? {
                let _ = self.cache.make_room(0);
            }
            return Ok(());
        }
        let Some(encoder) = self.cache.lock("encoder.lock")? else {
            return Ok(());
        };
        self.cache.make_room(RESERVATION)?;
        ensure!(
            free_bytes(&self.cache.root)? >= MIN_FREE_BYTES,
            "Insufficient seek cache space"
        );
        let output = tempfile::Builder::new()
            .prefix("rcr-partial-")
            .suffix(".mkv")
            .tempfile_in(&self.cache.root)?
            .into_temp_path();
        ensure!(
            !cancel.load(Ordering::Acquire),
            "Seek preparation cancelled"
        );
        let filter = proxy_filter(self.source_size);
        if self.trace {
            super::write_seek_trace(
                "Seek proxy trace",
                serde_json::json!({ "event": "encoder_start", "key": self.cache.key,
                    "program": std::env::var_os("RCR_FFMPEG").map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|| "ffmpeg".into()),
                    "source": self.source, "stream": self.stream,
                    "recipe": RECIPE, "filter": filter, "encoder_args": ENCODER_ARGS,
                    "file_budget": FILE_BUDGET, "emergency_limit": FILE_BUDGET + 8 * 1024 * 1024 })
            );
        }
        let mut child = media_command("ffmpeg")
            .args([
                "-nostdin",
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-hwaccel",
                "cuda",
                "-hwaccel_output_format",
                "cuda",
                "-i",
            ])
            .arg(&self.source)
            .args([
                "-map",
                &format!("0:{}", self.stream),
                "-an",
                "-sn",
                "-dn",
                "-vf",
                &filter,
            ])
            .args(ENCODER_ARGS)
            .args([
                "-fps_mode",
                "passthrough",
                // -fs can succeed after truncating; its emergency limit exceeds acceptance.
                "-fs",
                &(FILE_BUDGET + 8 * 1024 * 1024).to_string(),
                "-f",
                "matroska",
            ])
            .arg(&output)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(if self.trace { Stdio::piped() } else { Stdio::null() })
            .spawn()?;
        let job = match ProcessJob::attach(&child) {
            Ok(job) => job,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        };
        let (encoder, traced_encoder) = if self.trace {
            (None, Some(Arc::new(encoder)))
        } else {
            (Some(encoder), None)
        };
        let stderr_tail = self
            .trace
            .then(|| Arc::new(Mutex::new(VecDeque::with_capacity(STDERR_TAIL_BYTES))));
        let stderr = if self.trace {
            let Some(pipe) = child.stderr.take() else {
                let _ = child.kill();
                let _ = child.wait();
                drop(job);
                anyhow::bail!("Seek encoder stderr pipe unavailable");
            };
            let tail = stderr_tail.as_ref().unwrap().clone();
            let encoder_lease = traced_encoder.as_ref().unwrap().clone();
            match thread::Builder::new()
                .name("rcr-seek-encoder-stderr".into())
                .spawn(move || {
                    read_stderr_tail(pipe, &tail);
                    drop(encoder_lease);
                })
            {
                Ok(reader) => Some(reader),
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    drop(job);
                    return Err(error.into());
                }
            }
        } else {
            None
        };
        self.preparation = Some(Preparation {
            child,
            job: Some(job),
            _encoder: encoder,
            _traced_encoder: traced_encoder,
            output,
            started: Instant::now(),
            last_trace: Instant::now(),
            last_bytes: 0,
            trace_key: self.trace.then(|| self.cache.key.clone()),
            stderr,
            stderr_tail,
        });
        Ok(())
    }
    pub fn frame(
        &mut self,
        project: &Project,
        at: u64,
        size: (u32, u32),
        cancel: &AtomicBool,
    ) -> Result<Option<PreviewFrame>> {
        if !self.ready() || !self.matches(project) {
            return Ok(None);
        }
        let mut preview = project.clone();
        let Some(asset) = preview.assets.iter_mut().find(|a| a.id == self.asset) else {
            return Ok(None);
        };
        asset.path = self.cache.data();
        for clip in &mut preview.clips {
            if let Content::Media { asset, stream } = &mut clip.content {
                if *asset == self.asset && *stream == self.stream {
                    *stream = 0;
                }
            }
        }
        self.decoder.frame(&preview, at, size, cancel).map(Some)
    }
}
// A Windows kill-on-close job also reaps the encoder when the parent crashes.
struct ProcessJob(*mut std::ffi::c_void);
#[repr(C)]
#[derive(Default)]
struct BasicLimits {
    process_time: i64,
    job_time: i64,
    flags: u32,
    minimum_working_set: usize,
    maximum_working_set: usize,
    active_processes: u32,
    affinity: usize,
    priority_class: u32,
    scheduling_class: u32,
}
#[repr(C)]
#[derive(Default)]
struct ExtendedLimits {
    basic: BasicLimits,
    io_counters: [u64; 6],
    process_memory: usize,
    job_memory: usize,
    peak_process_memory: usize,
    peak_job_memory: usize,
}
#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateJobObjectW(
        attributes: *const std::ffi::c_void,
        name: *const u16,
    ) -> *mut std::ffi::c_void;
    fn SetInformationJobObject(
        job: *mut std::ffi::c_void,
        class: i32,
        info: *const std::ffi::c_void,
        length: u32,
    ) -> i32;
    fn AssignProcessToJobObject(job: *mut std::ffi::c_void, process: *mut std::ffi::c_void) -> i32;
    fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
}
impl ProcessJob {
    fn attach(child: &Child) -> Result<Self> {
        use std::os::windows::io::AsRawHandle;
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        ensure!(
            !handle.is_null(),
            "Cannot create seek encoder job: {}",
            std::io::Error::last_os_error()
        );
        let job = Self(handle);
        let mut limits = ExtendedLimits::default();
        limits.basic.flags = 0x2000;
        ensure!(
            unsafe {
                SetInformationJobObject(
                    handle,
                    9,
                    (&limits as *const ExtendedLimits).cast(),
                    std::mem::size_of::<ExtendedLimits>() as u32,
                )
            } != 0,
            "Cannot configure seek encoder job: {}",
            std::io::Error::last_os_error()
        );
        ensure!(
            unsafe { AssignProcessToJobObject(handle, child.as_raw_handle()) } != 0,
            "Cannot attach seek encoder job: {}",
            std::io::Error::last_os_error()
        );
        Ok(job)
    }
}
impl Drop for ProcessJob {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}
fn free_bytes(path: &Path) -> Result<u64> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetDiskFreeSpaceExW(
            path: *const u16,
            available: *mut u64,
            total: *mut u64,
            free: *mut u64,
        ) -> i32;
    }
    let path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut available = 0;
    ensure!(
        unsafe {
            GetDiskFreeSpaceExW(
                path.as_ptr(),
                &mut available,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        } != 0,
        "Cannot check seek cache disk space"
    );
    Ok(available)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn encoder_diagnostics_retain_only_the_final_bounded_bytes() {
        let bytes: Vec<_> = (0..STDERR_TAIL_BYTES * 3 + 37)
            .map(|index| (index % 251) as u8)
            .collect();
        let mut tail = VecDeque::new();
        for chunk in bytes.chunks(4096) {
            retain_tail(&mut tail, chunk);
            assert!(tail.len() <= STDERR_TAIL_BYTES);
        }
        assert_eq!(
            tail.into_iter().collect::<Vec<_>>(),
            bytes[bytes.len() - STDERR_TAIL_BYTES..]
        );
    }
    #[test]
    fn detail_recipe_never_upscales_or_trades_quality_for_a_bitrate_ceiling() {
        assert!(proxy_filter((640, 480)).starts_with("scale_cuda=640:480:"));
        assert!(proxy_filter((2560, 1440)).starts_with("scale_cuda=1920:1080:"));
        assert!(proxy_filter((1080, 1920)).starts_with("scale_cuda=1080:1080:"));
        let option = |name| {
            ENCODER_ARGS
                .windows(2)
                .find(|pair| pair[0] == name)
                .map(|pair| pair[1])
        };
        assert_eq!(option("-rc"), Some("constqp"));
        assert_eq!(option("-qp"), Some("24"));
        assert_eq!(option("-i_qfactor"), Some("1"));
        assert_eq!(option("-i_qoffset"), Some("0"));
        assert_eq!(option("-g"), Some("1"));
        assert_eq!(option("-bf"), Some("0"));
        assert_eq!(option("-slices"), Some("2"));
        assert_eq!(option("-b:v"), Some("0"));
        assert_eq!(option("-maxrate"), None);
        assert_eq!(option("-bufsize"), None);
        assert_eq!(FILE_BUDGET, 2 * 1024 * 1024 * 1024);
        assert_eq!(CACHE_BUDGET, 3 * 1024 * 1024 * 1024);
        assert_eq!(RESERVATION, FILE_BUDGET + 16 * 1024 * 1024);
        assert_eq!(MIN_FREE_BYTES, RESERVATION + 512 * 1024 * 1024);
    }
    fn identity(path: u16) -> Identity {
        Identity {
            path: vec![path],
            bytes: 100,
            modified_ns: 1_234_567_890,
            stream: 0,
            recipe: RECIPE,
        }
    }
    fn entry(root: &Path, identity: &Identity) -> Cache {
        Cache {
            root: root.to_path_buf(),
            key: identity.key().unwrap(),
        }
    }
    fn complete(cache: &Cache, identity: &Identity) {
        fs::write(cache.data(), [1u8; 64]).unwrap();
        fs::write(
            cache.marker(),
            serde_json::to_vec(&Complete {
                identity: identity.clone(),
                bytes: 64,
            })
            .unwrap(),
        )
        .unwrap();
    }
    fn bytes(cache: &Cache) -> u64 {
        cache.data().metadata().unwrap().len() + cache.marker().metadata().unwrap().len()
    }
    #[test]
    fn source_replacements_streams_and_recipe_changes_invalidate_cache_identity() {
        let original = identity(42);
        let key = original.key().unwrap();
        let mut variations = Vec::new();
        let mut changed = original.clone();
        changed.path.push(43);
        variations.push(changed);
        let mut changed = original.clone();
        changed.bytes += 1;
        variations.push(changed);
        let mut changed = original.clone();
        changed.modified_ns += 1;
        variations.push(changed);
        let mut changed = original.clone();
        changed.stream += 1;
        variations.push(changed);
        let mut changed = original.clone();
        changed.recipe += 1;
        variations.push(changed);
        for variation in variations {
            assert_ne!(key, variation.key().unwrap());
        }
        assert_eq!(key, original.clone().key().unwrap());
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source.mkv");
        fs::write(&source, [1, 2, 3]).unwrap();
        let before = Identity::read(&source, 0).unwrap();
        fs::write(&source, [1, 2, 3, 4]).unwrap();
        assert_ne!(before, Identity::read(&source, 0).unwrap());
    }
    #[test]
    fn incomplete_mismatched_and_truncated_entries_are_never_reused() {
        let root = tempfile::tempdir().unwrap();
        let identity = identity(1);
        let cache = entry(root.path(), &identity);
        fs::write(cache.data(), [1u8; 64]).unwrap();
        assert!(cache.reuse(&identity).unwrap().is_none());
        fs::write(cache.marker(), b"{incomplete").unwrap();
        assert!(cache.reuse(&identity).unwrap().is_none());
        complete(&cache, &identity);
        let mut other = identity.clone();
        other.modified_ns += 1;
        assert!(cache.reuse(&other).unwrap().is_none());
        fs::write(cache.data(), [1u8; 32]).unwrap();
        assert!(cache.reuse(&identity).unwrap().is_none());
        complete(&cache, &identity);
        assert!(cache.reuse(&identity).unwrap().is_some());
    }
    #[test]
    fn eviction_is_lru_but_active_readers_and_foreign_files_are_preserved() {
        let root = tempfile::tempdir().unwrap();
        let first_id = identity(1);
        let second_id = identity(2);
        let first = entry(root.path(), &first_id);
        let second = entry(root.path(), &second_id);
        complete(&first, &first_id);
        complete(&second, &second_id);
        OpenOptions::new()
            .write(true)
            .open(first.marker())
            .unwrap()
            .set_modified(UNIX_EPOCH + Duration::from_secs(10))
            .unwrap();
        OpenOptions::new()
            .write(true)
            .open(second.marker())
            .unwrap()
            .set_modified(UNIX_EPOCH + Duration::from_secs(20))
            .unwrap();
        let total = bytes(&first) + bytes(&second);
        let reserve = bytes(&first);
        let foreign = root.path().join("precious-original.mkv");
        fs::write(&foreign, b"original").unwrap();
        let lease = File::open(first.data()).unwrap();
        lease.try_lock_shared().unwrap();
        first.make_room_with_budget(reserve, total).unwrap();
        assert!(
            first.data().exists(),
            "Active native reader must survive eviction"
        );
        assert!(
            !second.data().exists(),
            "Evict the next oldest unpinned entry instead"
        );
        assert!(
            foreign.exists(),
            "Only generated hash entries may be removed"
        );
        drop(lease);
        first.make_room_with_budget(total, total).unwrap();
        assert!(!first.data().exists());
        assert!(foreign.exists());
    }
    #[test]
    fn orphan_cleanup_and_encoder_lock_prevent_parallel_preparation() {
        let root = tempfile::tempdir().unwrap();
        let id = identity(1);
        let cache = entry(root.path(), &id);
        let partial = root.path().join("rcr-partial-crashed.mkv");
        fs::write(&partial, b"unfinished").unwrap();
        fs::write(cache.data(), b"unpublished").unwrap();
        let guard = cache.lock("encoder.lock").unwrap().unwrap();
        assert!(cache.lock("encoder.lock").unwrap().is_none());
        cache.make_room(0).unwrap();
        assert!(!partial.exists());
        assert!(!cache.data().exists());
        drop(guard);
        assert!(cache.lock("encoder.lock").unwrap().is_some());
    }
    #[test]
    fn active_reservations_never_force_delete_locked_previews() {
        let root = tempfile::tempdir().unwrap();
        let id = identity(1);
        let cache = entry(root.path(), &id);
        complete(&cache, &id);
        let lease = cache.reuse(&id).unwrap().unwrap();
        assert!(cache.make_room_with_budget(1, 1).is_err());
        assert!(cache.data().exists());
        drop(lease);
        cache.make_room_with_budget(1, 1).unwrap();
        assert!(!cache.data().exists());
    }
}
