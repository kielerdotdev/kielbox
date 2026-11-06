//! Event-driven WASAPI sources. Each device has an independent named track.
//! GetBuffer QPC timestamps are converted onto the recorder's shared epoch.
use anyhow::{ensure, Context, Result};
use rcr_media_core::{
    AudioCapture, AudioSource, AudioTrack, MediaPacket, AUDIO_FRAME_BYTES, AUDIO_RATE,
    NS_PER_SECOND,
};
use std::{
    collections::VecDeque,
    ptr,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc, Arc, Condvar, Mutex,
    },
    thread,
    time::{Duration, Instant},
};
use windows::core::Interface;
use windows::Win32::{
    Foundation::{CloseHandle, FILETIME, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT},
    Media::Audio::*,
    System::{Com::*, Performance::*, Threading::*},
};

const ACTIVATION_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CaptureRoute {
    System,
    Microphone,
    Process {
        pid: u32,
        created_at_hns: Option<u64>,
    },
}
fn capture_route(source: &AudioSource) -> Result<CaptureRoute> {
    Ok(match source {
        AudioSource::System => CaptureRoute::System,
        AudioSource::Microphone => CaptureRoute::Microphone,
        AudioSource::Process {
            pid,
            created_at_hns,
        } => {
            ensure!(*pid > 0, "application audio requires a nonzero process ID");
            CaptureRoute::Process {
                pid: *pid,
                created_at_hns: *created_at_hns,
            }
        }
    })
}
fn validate_creation_time(expected: Option<u64>, actual: u64) -> Result<()> {
    ensure!(
        expected.is_none_or(|created| created == actual),
        "Selected application process changed; reselect it before recording"
    );
    Ok(())
}

struct CompletionState<T> {
    value: Option<T>,
    abandoned: bool,
}
struct Completion<T> {
    state: Mutex<CompletionState<T>>,
    wake: Condvar,
}
impl<T> Completion<T> {
    fn new() -> Self {
        Self {
            state: Mutex::new(CompletionState {
                value: None,
                abandoned: false,
            }),
            wake: Condvar::new(),
        }
    }
    fn publish(&self, value: T) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if !state.abandoned {
            state.value = Some(value);
            self.wake.notify_one();
        }
    }
    fn wait(&self, cancelled: &impl Fn() -> bool, timeout: Duration) -> Result<T> {
        let deadline = Instant::now() + timeout;
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            let stopped = cancelled();
            if stopped || Instant::now() >= deadline {
                state.abandoned = true;
                let discarded = state.value.take();
                drop(state);
                drop(discarded);
                anyhow::bail!(if stopped {
                    "application audio activation cancelled"
                } else {
                    "application audio activation timed out"
                });
            }
            if let Some(value) = state.value.take() {
                return Ok(value);
            }
            let remaining = deadline
                .saturating_duration_since(Instant::now())
                .min(Duration::from_millis(20));
            state = self
                .wake
                .wait_timeout(state, remaining)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }
}

fn validate_mta_kind(apartment: APTTYPE) -> Result<()> {
    ensure!(
        apartment == APTTYPE_MTA,
        "Application audio requires the process MTA, got {apartment:?}"
    );
    Ok(())
}
fn require_mta() -> Result<()> {
    let mut apartment = APTTYPE::default();
    let mut qualifier = APTTYPEQUALIFIER::default();
    unsafe { CoGetApartmentType(&mut apartment, &mut qualifier) }
        .context("Cannot verify application audio COM apartment")?;
    validate_mta_kind(apartment)
}

/// A single owned interface transfer within this process's MTA. This is NOT an
/// agile COM reference: the process-loopback client has no registered marshaler.
/// Microsoft permits direct pointer transfer between threads in the same MTA:
/// https://learn.microsoft.com/en-us/windows/win32/com/multithreaded-apartments
struct MtaClient(IAudioClient);
// SAFETY: private construction happens only after require_mta in ActivateCompleted.
// The sole receiver is process_client, also verified MTA before activation and
// consumption. Completion is never exposed outside these MTA contexts. Cancelled
// stored values drop on the worker before its ComApartment, while late abandoned
// values drop on the verified callback MTA. No Sync or Clone implementation permits
// concurrent use; Initialize/GetService/buffer access all stay on the worker.
unsafe impl Send for MtaClient {}
impl MtaClient {
    fn into_client(self) -> Result<IAudioClient> {
        require_mta()?;
        Ok(self.0)
    }
}

type ActivationResult = std::result::Result<MtaClient, String>;
// windows-rs implements IAgileObject and a free-threaded IMarshal by default.
// Backing parameters stay owned by this COM object until Windows releases it,
// including when the waiting worker has already cancelled or timed out.
#[windows::core::implement(IActivateAudioInterfaceCompletionHandler)]
struct ActivationHandler {
    completion: Arc<Completion<ActivationResult>>,
    params: Box<AUDIOCLIENT_ACTIVATION_PARAMS>,
}
impl IActivateAudioInterfaceCompletionHandler_Impl for ActivationHandler_Impl {
    fn ActivateCompleted(
        &self,
        operation: windows::core::Ref<'_, IActivateAudioInterfaceAsyncOperation>,
    ) -> windows::core::Result<()> {
        let result = (|| -> anyhow::Result<_> {
            // Check before acquiring any client, so a failed invariant cannot
            // hand a non-MTA-owned interface to the waiting capture worker.
            require_mta().context("activation completion callback apartment")?;
            let mut status = windows::core::HRESULT(0x80004005_u32 as i32);
            let mut interface = None;
            unsafe {
                operation
                    .ok()
                    .context("activation completion operation is null")?
                    .GetActivateResult(&mut status, &mut interface)
                    .context("GetActivateResult call failed")?;
            }
            status
                .ok()
                .context("GetActivateResult returned failed activation status")?;
            let client: IAudioClient = interface
                .context("GetActivateResult returned no interface")?
                .cast()
                .context("activated interface QueryInterface(IAudioClient) failed")?;
            Ok(MtaClient(client))
        })()
        .map_err(|error| format!("activate application audio: {error:#}"));
        self.completion.publish(result);
        Ok(())
    }
}

fn process_client(pid: u32, cancelled: &impl Fn() -> bool) -> Result<IAudioClient> {
    use std::mem::ManuallyDrop;
    use windows::Win32::System::{Com::StructuredStorage::*, Variant::VT_BLOB};
    require_mta().context("application capture worker apartment")?;
    let completion = Arc::new(Completion::new());
    let handler = ActivationHandler {
        completion: completion.clone(),
        params: Box::new(AUDIOCLIENT_ACTIVATION_PARAMS {
            ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
            Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
                ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                    TargetProcessId: pid,
                    ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
                },
            },
        }),
    };
    let params = (&*handler.params) as *const AUDIOCLIENT_ACTIVATION_PARAMS;
    let handler: IActivateAudioInterfaceCompletionHandler = handler.into();
    // This PROPVARIANT borrows Box storage; never PropVariantClear that storage.
    let variant = ManuallyDrop::new(PROPVARIANT {
        Anonymous: PROPVARIANT_0 {
            Anonymous: ManuallyDrop::new(PROPVARIANT_0_0 {
                vt: VT_BLOB,
                Anonymous: PROPVARIANT_0_0_0 {
                    blob: BLOB {
                        cbSize: std::mem::size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32,
                        pBlobData: params.cast_mut().cast(),
                    },
                },
                ..Default::default()
            }),
        },
    });
    ensure!(!cancelled(), "application audio activation cancelled");
    let _operation = unsafe { ActivateAudioInterfaceAsync(VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
        &IAudioClient::IID, Some((&*variant) as *const PROPVARIANT), &handler) }
        .context("Application audio requires supported Windows process loopback (documented build 20348+) and a running target; system audio was not substituted")?;
    let client = completion
        .wait(cancelled, ACTIVATION_TIMEOUT)?
        .map_err(anyhow::Error::msg)?;
    client.into_client()
}

struct ProcessTarget {
    handle: HANDLE,
    pid: u32,
    created: u64,
}
/// Call on a worker when selecting an application; pass this fingerprint to
/// AudioTrack::process_instance so a stale picker cannot capture a reused PID.
pub fn process_creation_time(pid: u32) -> Result<u64> {
    Ok(ProcessTarget::open(pid)?.created)
}
impl ProcessTarget {
    fn open(pid: u32) -> Result<Self> {
        ensure!(pid > 0, "application audio requires a nonzero process ID");
        let handle = unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                false,
                pid,
            )
        }
        .with_context(|| {
            format!(
                "Cannot inspect application process {pid}; choose a running accessible application"
            )
        })?;
        let mut target = Self {
            handle,
            pid,
            created: 0,
        };
        target.created = target.creation_time()?;
        target.ensure_running()?;
        Ok(target)
    }
    fn creation_time(&self) -> Result<u64> {
        let (mut creation, mut exit, mut kernel, mut user) = (
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
        );
        unsafe {
            GetProcessTimes(
                self.handle,
                &mut creation,
                &mut exit,
                &mut kernel,
                &mut user,
            )?;
        }
        Ok((u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime))
    }
    fn ensure_running(&self) -> Result<()> {
        ensure!(
            unsafe { WaitForSingleObject(self.handle, 0) } == WAIT_TIMEOUT,
            "Application process {} exited; application audio stopped (no system fallback)",
            self.pid
        );
        Ok(())
    }
    fn verify_identity(&self) -> Result<()> {
        self.ensure_running()?;
        let current = Self::open(self.pid)?;
        ensure!(
            current.created == self.created,
            "Application process identity changed; reselect the application"
        );
        Ok(())
    }
}
impl Drop for ProcessTarget {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

pub fn qpc_hns() -> Result<u64> {
    let mut counter = 0;
    let mut frequency = 0;
    unsafe {
        QueryPerformanceCounter(&mut counter)?;
        QueryPerformanceFrequency(&mut frequency)?;
    }
    Ok((counter as u128 * 10_000_000 / frequency as u128) as u64)
}

pub(crate) struct ComApartment;
impl ComApartment {
    pub(crate) fn new() -> Result<Self> {
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
        }
        Ok(Self)
    }
}
impl Drop for ComApartment {
    fn drop(&mut self) {
        unsafe {
            CoUninitialize();
        }
    }
}

pub enum AudioEvent {
    Packet(MediaPacket),
    Discontinuity,
    Notice(String),
    Error(String),
}

const RECOVERY_ATTEMPTS: usize = 3;
const RECOVERY_WINDOW: Duration = Duration::from_secs(5);
const MAX_AUDIO_NOTICES: usize = 8;

fn follows_default(track: &AudioTrack) -> bool {
    matches!(track.source, AudioSource::System) && track.endpoint_id.is_none()
}

fn recoverable_device_error(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<windows::core::Error>()
        .is_some_and(|error| {
            // DEVICE_INVALIDATED and RESOURCES_INVALIDATED only. Permission,
            // format, queue, application lifetime and microphone errors stay fatal.
            matches!(error.code().0 as u32, 0x8889_0004 | 0x8889_0026)
        })
}

fn queue_notice(notices: &Mutex<VecDeque<String>>, notice: String) {
    let mut notices = notices.lock().unwrap_or_else(|e| e.into_inner());
    if notices.len() == MAX_AUDIO_NOTICES {
        notices.pop_front();
    }
    notices.push_back(notice.chars().take(512).collect());
}

fn optional_default_watch<W>(
    track: &AudioTrack,
    notices: &Mutex<VecDeque<String>>,
    register: impl FnOnce() -> Result<W>,
) -> Option<W> {
    if !follows_default(track) {
        return None;
    }
    match register() {
        Ok(watch) => Some(watch),
        Err(error) => {
            queue_notice(notices, format!("{}: default-output change monitoring unavailable; capturing the current endpoint. Device invalidation can still retry the default output; restart recording if an output change is missed: {error:#}", track.name));
            None
        }
    }
}

/// Three reopen attempts per recording, at most five seconds of retry
/// scheduling per incident. Windows synchronous COM/driver calls themselves
/// cannot be given a hard timeout without leaking an unowned worker.
fn reopen_reader<R>(
    source: &mut Option<R>,
    attempts: &mut usize,
    cancelled: &impl Fn() -> bool,
    mut open: impl FnMut() -> Result<R>,
    mut pause: impl FnMut(),
    mut now: impl FnMut() -> Duration,
) -> Result<bool> {
    drop(source.take()); // Release the invalid client before activating another.
    let start = now();
    let mut last_error = None;
    while *attempts < RECOVERY_ATTEMPTS && now().saturating_sub(start) < RECOVERY_WINDOW {
        if cancelled() {
            return Ok(false);
        }
        pause();
        if cancelled() {
            return Ok(false);
        }
        if now().saturating_sub(start) >= RECOVERY_WINDOW {
            break;
        }
        *attempts += 1;
        match open() {
            Ok(reader) => {
                if cancelled() {
                    drop(reader);
                    return Ok(false);
                }
                if now().saturating_sub(start) >= RECOVERY_WINDOW {
                    drop(reader);
                    anyhow::bail!("Default desktop audio recovery exceeded its retry window; check the default output and restart recording (Windows activation may block inside its driver)");
                }
                *source = Some(reader);
                return Ok(true);
            }
            Err(error) => last_error = Some(error),
        }
    }
    anyhow::bail!("Default desktop audio could not recover within {RECOVERY_ATTEMPTS} reopen attempts / 5-second retry window; check the default output and restart recording: {}", last_error.map_or_else(|| "retry budget exhausted".into(), |e| format!("{e:#}")))
}

/// Only the first packets after a reopen need overlap trimming. Keep their
/// original QPC-derived PTS: a missing interval remains a real timeline gap.
#[derive(Default)]
struct RecoveryTimeline {
    last_end_ns: u64,
    joining: bool,
}
impl RecoveryTimeline {
    fn packet(&mut self, mut packet: MediaPacket) -> Option<MediaPacket> {
        if self.joining && packet.pts_ns < self.last_end_ns {
            let skip = (u128::from(self.last_end_ns - packet.pts_ns) * u128::from(AUDIO_RATE))
                .div_ceil(u128::from(NS_PER_SECOND));
            let frames = packet.data.len() / AUDIO_FRAME_BYTES;
            if skip >= frames as u128 {
                return None;
            }
            let skip = skip as usize;
            packet.data = packet.data[skip * AUDIO_FRAME_BYTES..].into();
            packet.pts_ns += (skip as u64 * NS_PER_SECOND).div_ceil(u64::from(AUDIO_RATE));
            packet.duration_ns = (frames - skip) as u64 * NS_PER_SECOND / u64::from(AUDIO_RATE);
        }
        self.joining = false;
        self.last_end_ns = self
            .last_end_ns
            .max(packet.pts_ns.saturating_add(packet.duration_ns));
        Some(packet)
    }
}

trait CaptureReader {
    fn wait_ready(&self) -> Result<()>;
    fn read(&mut self, epoch: u64) -> Result<Option<MediaPacket>>;
    fn discontinuities(&self) -> u64;
}
impl CaptureReader for WasapiCapture {
    fn wait_ready(&self) -> Result<()> {
        self.wait()
    }
    fn read(&mut self, epoch: u64) -> Result<Option<MediaPacket>> {
        self.next_packet(epoch)
    }
    fn discontinuities(&self) -> u64 {
        self.discontinuities
    }
}

fn read_available<R: CaptureReader>(
    source: &mut R,
    epoch_and_generation: (u64, u64),
    current_generation: impl Fn() -> u64,
    cancelled: &impl Fn() -> bool,
    timeline: &mut RecoveryTimeline,
    mut send: impl FnMut(MediaPacket) -> Result<()>,
) -> Result<bool> {
    let (epoch, generation) = epoch_and_generation;
    if current_generation() != generation {
        return Ok(true);
    }
    source.wait_ready()?;
    while !cancelled() {
        if current_generation() != generation {
            return Ok(true);
        }
        let Some(packet) = source.read(epoch)? else {
            break;
        };
        if let Some(packet) = timeline.packet(packet) {
            send(packet)?;
        }
    }
    Ok(false)
}

pub struct WasapiCapture {
    client: IAudioClient,
    capture: IAudioCaptureClient,
    event: HANDLE,
    track: u8,
    discontinuities: u64,
    started: bool,
    seen_packet: bool,
    process: Option<ProcessTarget>,
}
impl WasapiCapture {
    pub fn open(track: &AudioTrack) -> Result<Self> {
        Self::open_cancellable(track, &|| false)
    }
    fn open_cancellable(track: &AudioTrack, cancelled: &impl Fn() -> bool) -> Result<Self> {
        ensure!(!cancelled(), "audio startup cancelled");
        track.validate_endpoint_selection()?;
        let route = capture_route(&track.source)?;
        let process = if let CaptureRoute::Process {
            pid,
            created_at_hns,
        } = route
        {
            let process = ProcessTarget::open(pid)?;
            validate_creation_time(created_at_hns, process.created)?;
            Some(process)
        } else {
            None
        };
        unsafe {
            let client: IAudioClient = match route {
                CaptureRoute::Process { pid, .. } => {
                    process.as_ref().unwrap().verify_identity()?;
                    let client = process_client(pid, cancelled)?;
                    process.as_ref().unwrap().verify_identity()?;
                    client
                }
                CaptureRoute::System | CaptureRoute::Microphone => {
                    let enumerator: IMMDeviceEnumerator =
                        CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
                    let endpoint = crate::audio_endpoint::resolve_endpoint(&enumerator, track)?;
                    ensure!(!cancelled(), "audio startup cancelled");
                    endpoint.Activate(CLSCTX_ALL, None)?
                }
            };
            ensure!(!cancelled(), "audio startup cancelled");
            let format = WAVEFORMATEX {
                wFormatTag: 3,
                nChannels: 2,
                nSamplesPerSec: AUDIO_RATE,
                nAvgBytesPerSec: AUDIO_RATE * 8,
                nBlockAlign: 8,
                wBitsPerSample: 32,
                cbSize: 0,
            };
            let flags = AUDCLNT_STREAMFLAGS_EVENTCALLBACK
                | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
                | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY
                | if matches!(route, CaptureRoute::System | CaptureRoute::Process { .. }) {
                    AUDCLNT_STREAMFLAGS_LOOPBACK
                } else {
                    0
                };
            client
                .Initialize(AUDCLNT_SHAREMODE_SHARED, flags, 1_000_000, 0, &format, None)
                .context("initialise 48 kHz WASAPI shared capture")?;
            let capture = client.GetService()?;
            let event = CreateEventW(None, false, false, None)?;
            let mut result = Self {
                client,
                capture,
                event,
                track: track.id,
                discontinuities: 0,
                started: false,
                seen_packet: false,
                process,
            };
            result.client.SetEventHandle(event)?;
            ensure!(!cancelled(), "audio startup cancelled");
            result.client.Start()?;
            result.started = true;
            Ok(result)
        }
    }
    fn wait(&self) -> Result<()> {
        if let Some(process) = &self.process {
            process.ensure_running()?;
        }
        let status = unsafe { WaitForSingleObject(self.event, 20) };
        ensure!(
            status == WAIT_OBJECT_0 || status == WAIT_TIMEOUT,
            "audio wait failed: {status:?}"
        );
        Ok(())
    }
}
impl AudioCapture for WasapiCapture {
    fn next_packet(&mut self, epoch_hns: u64) -> Result<Option<MediaPacket>> {
        unsafe {
            if self.capture.GetNextPacketSize()? == 0 {
                return Ok(None);
            }
            let mut data = ptr::null_mut();
            let mut frames = 0;
            let mut flags = 0;
            let mut qpc = 0;
            let mut device = 0;
            self.capture.GetBuffer(
                &mut data,
                &mut frames,
                &mut flags,
                Some(&mut device),
                Some(&mut qpc),
            )?;
            // Copy PCM before releasing the WASAPI buffer; no raw video is involved.
            let bytes = frames as usize * AUDIO_FRAME_BYTES;
            let copied: Arc<[u8]> = if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 {
                vec![0; bytes].into()
            } else if !data.is_null() {
                std::slice::from_raw_parts(data, bytes).into()
            } else {
                self.capture.ReleaseBuffer(frames)?;
                anyhow::bail!("WASAPI returned a null non-silent buffer");
            };
            self.capture.ReleaseBuffer(frames)?;
            if self.seen_packet && flags & AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0 as u32 != 0 {
                self.discontinuities += 1;
            }
            self.seen_packet = true;
            if flags & AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR.0 as u32 != 0 {
                self.discontinuities += 1;
                qpc = qpc_hns()?
                    .saturating_sub(u64::from(frames) * 10_000_000 / u64::from(AUDIO_RATE));
            }
            let skip = (epoch_hns.saturating_sub(qpc) as u128 * u128::from(AUDIO_RATE))
                .div_ceil(10_000_000) as usize;
            if skip >= frames as usize {
                return Ok(None);
            }
            let data = if skip > 0 {
                copied[skip * AUDIO_FRAME_BYTES..].into()
            } else {
                copied
            };
            let first_hns = qpc + skip as u64 * 10_000_000 / u64::from(AUDIO_RATE);
            let pts_ns = first_hns.saturating_sub(epoch_hns) * 100;
            let duration_ns =
                (frames as usize - skip) as u64 * NS_PER_SECOND / u64::from(AUDIO_RATE);
            Ok(Some(MediaPacket {
                track: self.track,
                pts_ns,
                duration_ns,
                keyframe: true,
                data,
            }))
        }
    }
}
impl Drop for WasapiCapture {
    fn drop(&mut self) {
        unsafe {
            if self.started {
                let _ = self.client.Stop();
            }
            let _ = CloseHandle(self.event);
        }
    }
}

pub struct AudioWorkers {
    stop: Arc<AtomicBool>,
    threads: Vec<thread::JoinHandle<()>>,
    pub receiver: AudioReceiver,
    epoch: Arc<AtomicU64>,
}
/// Terminal failures have separate storage, so a full PCM queue cannot swallow
/// the very error reporting that its audio source has stopped.
pub struct AudioReceiver {
    packets: mpsc::Receiver<AudioEvent>,
    errors: Arc<Mutex<Vec<String>>>,
    notices: Arc<Mutex<VecDeque<String>>>,
    discontinuities: Arc<AtomicU64>,
}
impl AudioReceiver {
    pub fn try_iter(&self) -> impl Iterator<Item = AudioEvent> + '_ {
        let errors = std::mem::take(&mut *self.errors.lock().unwrap_or_else(|e| e.into_inner()));
        let notices = std::mem::take(&mut *self.notices.lock().unwrap_or_else(|e| e.into_inner()));
        let discontinuities = self.discontinuities.swap(0, Ordering::AcqRel);
        notices
            .into_iter()
            .map(AudioEvent::Notice)
            .chain(errors.into_iter().map(AudioEvent::Error))
            .chain((0..discontinuities).map(|_| AudioEvent::Discontinuity))
            .chain(self.packets.try_iter())
    }
}
impl AudioWorkers {
    pub fn start(tracks: &[AudioTrack], epoch_hns: u64) -> Result<Self> {
        Self::start_cancellable(tracks, epoch_hns, Arc::new(AtomicBool::new(false)))
    }
    pub fn start_cancellable(
        tracks: &[AudioTrack],
        epoch_hns: u64,
        external_stop: Arc<AtomicBool>,
    ) -> Result<Self> {
        ensure!(
            !external_stop.load(Ordering::Relaxed),
            "audio startup cancelled"
        );
        for track in tracks {
            track.validate_endpoint_selection()?;
        }
        let (tx, packets) = mpsc::sync_channel(512);
        let errors = Arc::new(Mutex::new(Vec::new()));
        let notices = Arc::new(Mutex::new(VecDeque::new()));
        let discontinuities = Arc::new(AtomicU64::new(0));
        let receiver = AudioReceiver {
            packets,
            errors: errors.clone(),
            notices: notices.clone(),
            discontinuities: discontinuities.clone(),
        };
        let (ready_tx, ready_rx) =
            mpsc::sync_channel::<std::result::Result<(), String>>(tracks.len().max(1));
        let mut result = Self {
            stop: Arc::new(AtomicBool::new(false)),
            threads: Vec::new(),
            receiver,
            epoch: Arc::new(AtomicU64::new(epoch_hns)),
        };
        for track in tracks.iter().cloned() {
            let stop = result.stop.clone();
            let tx = tx.clone();
            let ready = ready_tx.clone();
            let epoch = result.epoch.clone();
            let errors = errors.clone();
            let notices = notices.clone();
            let discontinuities = discontinuities.clone();
            let external_stop = external_stop.clone();
            result.threads.push(
                thread::Builder::new()
                    .name(format!("rcr-audio-{}", track.id))
                    .spawn(move || {
                        let mut ready_sent = false;
                        let mut run = || -> Result<()> {
                            let cancelled = || {
                                stop.load(Ordering::Relaxed)
                                    || external_stop.load(Ordering::Relaxed)
                            };
                            let _com = ComApartment::new()?;
                            let follow_default = follows_default(&track);
                            let watch = optional_default_watch(&track, &notices, crate::audio_endpoint::DefaultRenderWatch::start);
                            let generation = || watch.as_ref().map_or(0, |watch| watch.generation());
                            // Read BEFORE open, so a default change during activation
                            // is noticed even if activation happened to choose the old device.
                            let mut source_generation = generation();
                            let mut source = Some(WasapiCapture::open_cancellable(&track, &cancelled)?);
                            let _ = ready.send(Ok(()));
                            ready_sent = true;
                            let mut previous_discontinuities = 0;
                            let mut timeline = RecoveryTimeline::default();
                            let mut attempts = 0;
                            let clock = Instant::now();
                            while !cancelled() {
                                let epoch = epoch.load(Ordering::Acquire);
                                if epoch == 0 {
                                    thread::sleep(Duration::from_millis(1));
                                    continue;
                                }
                                let reader = source.as_mut().expect("active audio reader");
                                let result = read_available(reader, (epoch, source_generation), generation, &cancelled, &mut timeline, |packet| {
                                    tx.try_send(AudioEvent::Packet(packet))
                                        .context("audio queue overflow or recorder disconnected")
                                });
                                let observed = reader.discontinuities();
                                discontinuities.fetch_add(observed.saturating_sub(previous_discontinuities), Ordering::Relaxed);
                                previous_discontinuities = observed;
                                if cancelled() { break; }
                                let reason = match result {
                                    Ok(false) => continue,
                                    Ok(true) => "Windows default desktop output changed".to_string(),
                                    Err(error) if follow_default && recoverable_device_error(&error) => format!("Windows default desktop audio was invalidated: {error:#}"),
                                    Err(error) => return Err(error).context("Audio stopped; restore or reselect the selected source and restart recording; no other source was substituted"),
                                };
                                queue_notice(&notices, format!("{}: {reason}; attempting recovery, missing audio remains a timeline gap", track.name));
                                discontinuities.fetch_add(1, Ordering::Relaxed);
                                // Sample the notification generation for every reopen,
                                // including unsuccessful ones. A later notification
                                // during activation is picked up on the next poll.
                                let recovered = reopen_reader(&mut source, &mut attempts, &cancelled, || {
                                    source_generation = generation();
                                    WasapiCapture::open_cancellable(&track, &cancelled)
                                }, || {
                                    for _ in 0..10 {
                                        if cancelled() { break; }
                                        thread::sleep(Duration::from_millis(10));
                                    }
                                }, || clock.elapsed())?;
                                if !recovered { break; }
                                timeline.joining = true;
                                previous_discontinuities = 0;
                                queue_notice(&notices, format!("{}: default desktop audio recovered; original recording clock retained", track.name));
                            }
                            Ok(())
                        };
                        if let Err(error) = run() {
                            let detail = format!("{}: {error:#}", track.name);
                            errors
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .push(detail.clone());
                            if !ready_sent {
                                let _ = ready.send(Err(detail));
                            }
                        }
                    })?,
            );
        }
        for _ in tracks {
            ready_rx
                .recv_timeout(Duration::from_secs(4))
                .context("audio device startup timed out")?
                .map_err(anyhow::Error::msg)?;
        }
        Ok(result)
    }
    pub fn set_epoch(&self, epoch: u64) {
        self.epoch.store(epoch, Ordering::Release);
    }
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}
impl Drop for AudioWorkers {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn pcm(pts_ns: u64, frames: usize) -> MediaPacket {
        MediaPacket {
            track: 2,
            pts_ns,
            duration_ns: frames as u64 * NS_PER_SECOND / u64::from(AUDIO_RATE),
            keyframe: true,
            data: (0..frames)
                .flat_map(|frame| (frame as u64).to_le_bytes())
                .collect::<Vec<_>>()
                .into(),
        }
    }

    #[test]
    fn recovery_is_only_default_system_and_only_typed_invalidation() {
        assert!(follows_default(&AudioTrack::system()));
        assert!(!follows_default(
            &AudioTrack::system().with_endpoint("opaque-id").unwrap()
        ));
        assert!(!follows_default(&AudioTrack::microphone()));
        assert!(!follows_default(&AudioTrack::process(42).unwrap()));
        for code in [0x8889_0004_u32, 0x8889_0026] {
            let error: anyhow::Error =
                windows::core::Error::from_hresult(windows::core::HRESULT(code as i32)).into();
            assert!(recoverable_device_error(&error.context("capture read")));
        }
        for code in [0x8007_0005_u32, 0x8889_0008, 0x8889_0010] {
            let error: anyhow::Error =
                windows::core::Error::from_hresult(windows::core::HRESULT(code as i32)).into();
            assert!(!recoverable_device_error(&error));
        }
        assert!(!recoverable_device_error(&anyhow::anyhow!(
            "device invalidated"
        )));
    }

    #[test]
    fn recovery_trims_overlap_but_keeps_qpc_gap_and_pcm_values() {
        let mut timeline = RecoveryTimeline::default();
        timeline.packet(pcm(1_000_000_000, 480)).unwrap();
        timeline.joining = true;
        assert!(timeline.packet(pcm(900_000_000, 480)).is_none());
        let joined = timeline.packet(pcm(1_005_000_000, 480)).unwrap();
        assert_eq!(joined.pts_ns, 1_010_000_000);
        assert_eq!(joined.duration_ns, 5_000_000);
        assert_eq!(joined.data.len(), 240 * AUDIO_FRAME_BYTES);
        assert_eq!(
            u64::from_le_bytes(joined.data[..8].try_into().unwrap()),
            240
        );
        assert_eq!(timeline.last_end_ns, 1_015_000_000);
        timeline.joining = true;
        let gap = timeline.packet(pcm(1_115_000_000, 480)).unwrap();
        assert_eq!(gap.pts_ns - 1_015_000_000, 100_000_000);
        assert_eq!(gap.data.len(), 480 * AUDIO_FRAME_BYTES);
        // Normal QPC quantization is not rounded/rebased or trimmed.
        let normal = pcm(1_124_999_900, 480);
        assert_eq!(timeline.packet(normal.clone()).unwrap().data, normal.data);
    }

    struct OwnedReader<'a>(&'a Cell<usize>);
    impl Drop for OwnedReader<'_> {
        fn drop(&mut self) {
            self.0.set(self.0.get() - 1);
        }
    }

    #[test]
    fn reopen_releases_old_reader_and_bounds_retries_across_incidents() {
        let active = Cell::new(1);
        let mut source = Some(OwnedReader(&active));
        let mut attempts = 0;
        let calls = Cell::new(0);
        assert!(reopen_reader(
            &mut source,
            &mut attempts,
            &|| false,
            || {
                assert_eq!(active.get(), 0, "old client remains active while reopening");
                calls.set(calls.get() + 1);
                if calls.get() < 3 {
                    anyhow::bail!("default temporarily absent");
                }
                active.set(1);
                Ok(OwnedReader(&active))
            },
            || {},
            || Duration::ZERO
        )
        .unwrap());
        assert_eq!(attempts, 3);
        assert_eq!(active.get(), 1);
        assert!(reopen_reader(
            &mut source,
            &mut attempts,
            &|| false,
            || panic!("budget allowed fourth open"),
            || {},
            || Duration::ZERO
        )
        .is_err());
        assert!(source.is_none());
        assert_eq!(active.get(), 0);
    }

    #[test]
    fn reopen_cancel_before_during_pause_and_after_open_releases_every_reader() {
        for stage in 0..3 {
            let active = Cell::new(1);
            let stop = Cell::new(stage == 0);
            let mut source = Some(OwnedReader(&active));
            let mut attempts = 0;
            let result = reopen_reader(
                &mut source,
                &mut attempts,
                &|| stop.get(),
                || {
                    assert_eq!(stage, 2);
                    assert_eq!(active.get(), 0);
                    active.set(1);
                    stop.set(true);
                    Ok(OwnedReader(&active))
                },
                || {
                    if stage == 1 {
                        stop.set(true);
                    }
                },
                || Duration::ZERO,
            )
            .unwrap();
            assert!(!result);
            assert!(source.is_none());
            assert_eq!(active.get(), 0);
            assert_eq!(attempts, usize::from(stage == 2));
        }
    }

    #[test]
    fn reopen_window_stops_scheduling_and_rejects_late_success_without_claiming_driver_timeout() {
        let clock = Cell::new(Duration::ZERO);
        let mut source: Option<()> = None;
        let mut attempts = 0;
        assert!(reopen_reader(
            &mut source,
            &mut attempts,
            &|| false,
            || panic!("opened after deadline"),
            || clock.set(RECOVERY_WINDOW),
            || clock.get()
        )
        .is_err());
        assert_eq!(attempts, 0);
        let active = Cell::new(0);
        clock.set(Duration::ZERO);
        let mut source = None;
        assert!(reopen_reader(
            &mut source,
            &mut attempts,
            &|| false,
            || {
                active.set(1);
                clock.set(RECOVERY_WINDOW + Duration::from_secs(1));
                Ok(OwnedReader(&active))
            },
            || {},
            || clock.get()
        )
        .is_err());
        assert!(source.is_none());
        assert_eq!(active.get(), 0);
        assert_eq!(attempts, 1);
    }

    struct InjectedReader {
        reads: usize,
        expected_epoch: u64,
        invalid: bool,
    }
    impl CaptureReader for InjectedReader {
        fn wait_ready(&self) -> Result<()> {
            Ok(())
        }
        fn read(&mut self, epoch: u64) -> Result<Option<MediaPacket>> {
            assert_eq!(epoch, self.expected_epoch, "recording QPC epoch changed");
            if self.invalid {
                return Err(windows::core::Error::from_hresult(windows::core::HRESULT(
                    0x8889_0004_u32 as i32,
                ))
                .into());
            }
            self.reads += 1;
            Ok((self.reads == 1).then(|| pcm(2_000_000_000, 480)))
        }
        fn discontinuities(&self) -> u64 {
            0
        }
    }

    #[test]
    fn injected_reader_detects_change_before_old_data_and_preserves_epoch_after_reopen() {
        let mut reader = InjectedReader {
            reads: 0,
            expected_epoch: 12345,
            invalid: false,
        };
        let mut timeline = RecoveryTimeline::default();
        let packets = Cell::new(0);
        assert!(read_available(
            &mut reader,
            (12345, 1),
            || 2,
            &|| false,
            &mut timeline,
            |_| panic!("read stale default")
        )
        .unwrap());
        assert_eq!(reader.reads, 0);
        assert!(!read_available(
            &mut reader,
            (12345, 2),
            || 2,
            &|| false,
            &mut timeline,
            |_| {
                packets.set(packets.get() + 1);
                Ok(())
            }
        )
        .unwrap());
        assert_eq!(packets.get(), 1);
        reader.invalid = true;
        let error = read_available(
            &mut reader,
            (12345, 2),
            || 2,
            &|| false,
            &mut timeline,
            |_| Ok(()),
        )
        .unwrap_err();
        assert!(recoverable_device_error(&error));
    }

    #[test]
    fn failed_optional_notification_registration_does_not_stop_or_replace_audio() {
        let notices = Mutex::new(VecDeque::new());
        let track = AudioTrack::system();
        let watch: Option<()> =
            optional_default_watch(&track, &notices, || anyhow::bail!("registration denied"));
        assert!(watch.is_none());
        assert!(
            follows_default(&track),
            "typed invalidation recovery must remain eligible without a watch"
        );
        assert!(notices
            .lock()
            .unwrap()
            .front()
            .unwrap()
            .contains("monitoring unavailable"));
        let mut source = InjectedReader {
            reads: 0,
            expected_epoch: 12345,
            invalid: false,
        };
        let mut packets = Vec::new();
        assert!(!read_available(
            &mut source,
            (12345, 0),
            || 0,
            &|| false,
            &mut RecoveryTimeline::default(),
            |packet| {
                packets.push(packet);
                Ok(())
            }
        )
        .unwrap());
        assert_eq!(packets.len(), 1);
        assert_eq!(packets[0].pts_ns, 2_000_000_000);
        for track in [
            AudioTrack::microphone(),
            AudioTrack::system().with_endpoint("selected").unwrap(),
            AudioTrack::process(42).unwrap(),
        ] {
            let watch: Option<()> = optional_default_watch(&track, &notices, || {
                panic!("strict route registered a default follower")
            });
            assert!(watch.is_none());
            assert!(!follows_default(&track));
        }
    }

    #[test]
    fn recovery_notices_are_bounded_and_terminal_error_wins_when_pcm_is_full() {
        let (tx, packets) = mpsc::sync_channel(1);
        tx.try_send(AudioEvent::Packet(pcm(0, 480))).ok().unwrap();
        let notices = Arc::new(Mutex::new(VecDeque::new()));
        for i in 0..100 {
            queue_notice(&notices, format!("{i}:{}", "x".repeat(1024)));
        }
        let receiver = AudioReceiver {
            packets,
            notices,
            errors: Arc::new(Mutex::new(vec!["recovery exhausted".into()])),
            discontinuities: Arc::new(AtomicU64::new(3)),
        };
        let events = receiver.try_iter().collect::<Vec<_>>();
        assert_eq!(events.len(), MAX_AUDIO_NOTICES + 1 + 3 + 1);
        assert!(events[..MAX_AUDIO_NOTICES]
            .iter()
            .all(|event| matches!(event, AudioEvent::Notice(detail) if detail.len()==512)));
        assert!(
            matches!(&events[MAX_AUDIO_NOTICES], AudioEvent::Error(detail) if detail=="recovery exhausted")
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, AudioEvent::Discontinuity))
                .count(),
            3
        );
        assert_eq!(receiver.try_iter().count(), 0);
    }
    #[test]
    fn source_dispatch_is_exhaustive_and_process_is_never_a_microphone() {
        assert_eq!(
            capture_route(&AudioSource::System).unwrap(),
            CaptureRoute::System
        );
        assert_eq!(
            capture_route(&AudioSource::Microphone).unwrap(),
            CaptureRoute::Microphone
        );
        assert_eq!(
            capture_route(&AudioSource::Process {
                pid: 42,
                created_at_hns: Some(7)
            })
            .unwrap(),
            CaptureRoute::Process {
                pid: 42,
                created_at_hns: Some(7)
            }
        );
        assert!(capture_route(&AudioSource::Process {
            pid: 0,
            created_at_hns: None
        })
        .is_err());
        validate_creation_time(None, 42).unwrap();
        validate_creation_time(Some(42), 42).unwrap();
        assert!(validate_creation_time(Some(41), 42).is_err());
    }
    #[test]
    fn handoff_requires_mta_and_rejects_sta_or_neutral_apartments() {
        validate_mta_kind(APTTYPE_MTA).unwrap();
        assert!(validate_mta_kind(APTTYPE_STA).is_err());
        assert!(validate_mta_kind(APTTYPE_MAINSTA).is_err());
        assert!(validate_mta_kind(APTTYPE_NA).is_err());
        fn assert_send<T: Send>() {}
        assert_send::<MtaClient>();
    }
    #[test]
    fn activation_completion_handles_ready_cancelled_timeout_and_late_results() {
        let complete = Completion::new();
        complete.publish(42);
        assert_eq!(
            complete.wait(&|| false, Duration::from_secs(1)).unwrap(),
            42
        );
        let cancelled = Completion::new();
        cancelled.publish(7);
        assert!(cancelled.wait(&|| true, Duration::from_secs(1)).is_err());
        assert!(cancelled.state.lock().unwrap().value.is_none());
        let timed_out = Completion::new();
        assert!(timed_out.wait(&|| false, Duration::ZERO).is_err());
        timed_out.publish(9);
        assert!(
            timed_out.state.lock().unwrap().value.is_none(),
            "Late callback retained an abandoned client"
        );
    }
    #[test]
    fn terminal_error_survives_a_full_pcm_queue_and_is_delivered_once() {
        let (tx, packets) = mpsc::sync_channel(1);
        assert!(tx.try_send(AudioEvent::Discontinuity).is_ok());
        assert!(tx.try_send(AudioEvent::Discontinuity).is_err());
        let errors = Arc::new(Mutex::new(vec![
            "application audio stopped: PCM queue overflow".into(),
        ]));
        let receiver = AudioReceiver {
            packets,
            errors,
            notices: Arc::default(),
            discontinuities: Arc::default(),
        };
        let events = receiver.try_iter().collect::<Vec<_>>();
        assert!(matches!(&events[0], AudioEvent::Error(detail) if detail.contains("overflow")));
        assert!(matches!(events[1], AudioEvent::Discontinuity));
        assert_eq!(receiver.try_iter().count(), 0);
    }
    #[test]
    fn external_stop_wakes_activation_without_waiting_for_a_callback() {
        let stop = Arc::new(AtomicBool::new(false));
        let completion = Arc::new(Completion::<u32>::new());
        let signal = stop.clone();
        let waiting = completion.clone();
        let (tx, rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            let result = waiting.wait(&|| signal.load(Ordering::Relaxed), Duration::from_secs(3));
            tx.send(result.is_err()).unwrap();
        });
        stop.store(true, Ordering::Relaxed);
        assert!(rx
            .recv_timeout(Duration::from_secs(1))
            .expect("External cancellation did not wake activation"));
        worker.join().unwrap();
        completion.publish(123);
        assert!(completion.state.lock().unwrap().value.is_none());
        assert!(AudioWorkers::start_cancellable(&[], 0, stop).is_err());
    }
}
