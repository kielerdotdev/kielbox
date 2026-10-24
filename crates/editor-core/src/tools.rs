use crate::{MediaAsset, MediaStream, TrackKind};
use anyhow::{ensure, Context, Result};
use serde::Deserialize;
use std::{
    io::Read,
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Output, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    thread,
    time::Duration,
};

pub fn media_command(program: &str) -> Command {
    let key = match program {
        "ffmpeg" => "RCR_FFMPEG",
        "ffprobe" => "RCR_FFPROBE",
        _ => "RCR_FFPLAY",
    };
    let mut command = Command::new(std::env::var_os(key).unwrap_or_else(|| program.into()));
    // Media jobs never own the application's terminal input. Callers piping
    // audio to FFplay explicitly replace this with their decoder's stdout.
    command.stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000 | 0x00004000);
    }
    command
}

struct ToolChild(Child);
impl Drop for ToolChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn bounded_pipe(mut pipe: impl Read, limit: u64) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    pipe.by_ref().take(limit + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= limit,
        "Media tool output exceeded its limit"
    );
    Ok(bytes)
}

fn cancellable_output(command: &mut Command, cancelled: impl Fn() -> bool) -> Result<Output> {
    ensure!(!cancelled(), "Media task cancelled");
    let mut child = ToolChild(
        command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?,
    );
    let stdout = child.0.stdout.take().context("Media output pipe missing")?;
    let stderr = child.0.stderr.take().context("Media error pipe missing")?;
    let failed = Arc::new(AtomicBool::new(false));
    let output_failed = failed.clone();
    let output = thread::Builder::new()
        .name("rcr-tool-output".into())
        .spawn(move || {
            let result = bounded_pipe(stdout, 4 * 1024 * 1024);
            if result.is_err() {
                output_failed.store(true, Ordering::Release);
            }
            result
        })?;
    let error_failed = failed.clone();
    let errors = match thread::Builder::new()
        .name("rcr-tool-errors".into())
        .spawn(move || {
            let result = bounded_pipe(stderr, 1024 * 1024);
            if result.is_err() {
                error_failed.store(true, Ordering::Release);
            }
            result
        }) {
        Ok(reader) => reader,
        Err(error) => {
            let _ = child.0.kill();
            let _ = child.0.wait();
            let _ = output.join();
            return Err(error.into());
        }
    };
    let status = (|| -> Result<_> {
        loop {
            ensure!(!cancelled(), "Media task cancelled");
            ensure!(
                !failed.load(Ordering::Acquire),
                "Media output reader failed"
            );
            if let Some(status) = child.0.try_wait()? {
                return Ok(status);
            }
            thread::sleep(Duration::from_millis(10));
        }
    })();
    if status.is_err() {
        let _ = child.0.kill();
        let _ = child.0.wait();
    }
    let stdout = output.join();
    let stderr = errors.join();
    let stdout = stdout.map_err(|_| anyhow::anyhow!("Media output reader panicked"))?;
    let stderr = stderr.map_err(|_| anyhow::anyhow!("Media error reader panicked"))?;
    let stdout = stdout?;
    let stderr = stderr?;
    Ok(Output {
        status: status?,
        stdout,
        stderr,
    })
}

/// Consume bounded stdout chunks while this thread retains ownership of the
/// child. Blocking pipe reads must never prevent cancellation from killing it.
pub(crate) fn stream_media_output(
    command: &mut Command,
    chunk_bytes: usize,
    cancelled: impl Fn() -> bool,
    mut consume: impl FnMut(&[u8]) -> Result<()>,
) -> Result<ExitStatus> {
    ensure!(!cancelled(), "Media task cancelled");
    ensure!(
        (1..=1024 * 1024).contains(&chunk_bytes),
        "invalid media chunk size"
    );
    let mut child = ToolChild(command.stdout(Stdio::piped()).spawn()?);
    let mut pipe = child.0.stdout.take().context("Media stream pipe missing")?;
    let (tx, rx) = mpsc::sync_channel(2);
    let reader = thread::Builder::new()
        .name("rcr-media-stream".into())
        .spawn(move || {
            let read = (|| -> Result<()> {
                loop {
                    let mut bytes = vec![0; chunk_bytes];
                    let mut count = 0;
                    while count < bytes.len() {
                        let got = pipe.read(&mut bytes[count..])?;
                        if got == 0 {
                            break;
                        }
                        count += got;
                    }
                    if count == 0 {
                        return Ok(());
                    }
                    bytes.truncate(count);
                    tx.send(Ok(bytes))
                        .map_err(|_| anyhow::anyhow!("Media stream cancelled"))?;
                }
            })();
            if let Err(error) = read {
                let _ = tx.send(Err(error));
            }
        })?;
    let result = (|| -> Result<ExitStatus> {
        loop {
            ensure!(!cancelled(), "Media task cancelled");
            match rx.recv_timeout(Duration::from_millis(10)) {
                Ok(bytes) => consume(&bytes?)?,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        loop {
            ensure!(!cancelled(), "Media task cancelled");
            if let Some(status) = child.0.try_wait()? {
                return Ok(status);
            }
            thread::sleep(Duration::from_millis(10));
        }
    })();
    if result.is_err() {
        let _ = child.0.kill();
        let _ = child.0.wait();
    }
    drop(rx);
    reader
        .join()
        .map_err(|_| anyhow::anyhow!("Media stream reader panicked"))?;
    result
}

pub fn probe(path: &Path) -> Result<MediaAsset> {
    probe_cancellable(path, || false)
}

pub fn probe_cancellable(path: &Path, cancelled: impl Fn() -> bool) -> Result<MediaAsset> {
    ensure!(!cancelled(), "Media task cancelled");
    #[derive(Deserialize)]
    struct Response {
        streams: Vec<Stream>,
        format: Format,
    }
    #[derive(Deserialize)]
    struct Format {
        duration: Option<String>,
    }
    #[derive(Deserialize)]
    struct Stream {
        index: u32,
        codec_type: String,
        width: Option<u32>,
        height: Option<u32>,
        duration: Option<String>,
        tags: Option<Tags>,
    }
    #[derive(Deserialize)]
    struct Tags {
        title: Option<String>,
        handler_name: Option<String>,
    }
    let path = path.canonicalize().context("media file is unavailable")?;
    let source_stamp = crate::SourceStamp::read(&path)?;
    let output=cancellable_output(media_command("ffprobe").args(["-v","error","-show_entries",
        "format=duration:stream=index,codec_type,width,height,duration:stream_tags=title,handler_name","-of","json","-i"])
        .arg(&path), &cancelled).context("FFprobe is required for importing media; install it or set RCR_FFPROBE")?;
    ensure!(
        output.status.success(),
        "cannot probe media: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    ensure!(
        output.stdout.len() < 4 * 1024 * 1024,
        "media metadata is too large"
    );
    let response: Response = serde_json::from_slice(&output.stdout)?;
    let duration = response
        .format
        .duration
        .as_ref()
        .and_then(|d| d.parse::<f64>().ok())
        .or_else(|| {
            response
                .streams
                .iter()
                .filter_map(|s| s.duration.as_ref()?.parse::<f64>().ok())
                .max_by(f64::total_cmp)
        })
        .context("media duration is unknown")?;
    ensure!(
        duration.is_finite() && duration > 0. && duration <= 86400.,
        "unsupported media duration"
    );
    let video = response.streams.iter().find(|s| s.codec_type == "video");
    let (width, height) = video.map_or((0, 0), |v| (v.width.unwrap_or(0), v.height.unwrap_or(0)));
    let streams = response
        .streams
        .into_iter()
        .filter_map(|s| {
            let kind = match s.codec_type.as_str() {
                "video" => TrackKind::Video,
                "audio" => TrackKind::Audio,
                _ => return None,
            };
            Some(MediaStream {
                index: s.index,
                kind,
                name: s
                    .tags
                    .and_then(|t| t.title.or(t.handler_name))
                    .unwrap_or_else(|| {
                        format!(
                            "{} {}",
                            if kind == TrackKind::Video {
                                "Video"
                            } else {
                                "Audio"
                            },
                            s.index
                        )
                    }),
            })
        })
        .collect::<Vec<_>>();
    ensure!(!streams.is_empty(), "media has no supported streams");
    ensure!(!cancelled(), "Media task cancelled");
    ensure!(
        crate::SourceStamp::read(&path)? == source_stamp,
        "Source changed while reading metadata: {}",
        path.display()
    );
    Ok(MediaAsset {
        id: 0,
        path,
        source_stamp: Some(source_stamp),
        duration_ns: (duration * 1e9).round() as u64,
        width,
        height,
        streams,
    })
}

#[derive(Clone, Copy, Debug, Default)]
pub struct WaveBin {
    pub min: f32,
    pub max: f32,
    pub rms: f32,
}

/// Small, seeked source thumbnails; avoids decoding an entire recording for a strip.
pub fn thumbnails(asset: &MediaAsset) -> Result<Vec<crate::PreviewFrame>> {
    thumbnails_cancellable(asset, || false)
}

pub fn thumbnails_cancellable(
    asset: &MediaAsset,
    cancelled: impl Fn() -> bool,
) -> Result<Vec<crate::PreviewFrame>> {
    let mut frames = Vec::new();
    for i in 0..4 {
        ensure!(!cancelled(), "Media task cancelled");
        frames.push(thumbnail_cancellable(
            asset,
            (i as f64 + 0.25) / 4.,
            (160, 90),
            &cancelled,
        )?);
    }
    Ok(frames)
}

/// One seeked thumbnail for the library, with bounded decode and output size.
pub fn thumbnail(
    asset: &MediaAsset,
    fraction: f64,
    size: (u32, u32),
) -> Result<crate::PreviewFrame> {
    thumbnail_cancellable(asset, fraction, size, || false)
}

pub fn thumbnail_cancellable(
    asset: &MediaAsset,
    fraction: f64,
    size: (u32, u32),
    cancelled: impl Fn() -> bool,
) -> Result<crate::PreviewFrame> {
    ensure!(!cancelled(), "Media task cancelled");
    ensure!(
        size.0 > 0 && size.1 > 0 && size.0 <= 640 && size.1 <= 360,
        "Invalid thumbnail size"
    );
    let stream = asset
        .streams
        .iter()
        .find(|s| s.kind == TrackKind::Video)
        .context("Recording has no video")?;
    let seconds = asset.duration_ns as f64 / 1e9 * fraction.clamp(0., 0.95);
    let output = cancellable_output(
        media_command("ffmpeg")
            .args([
                "-v",
                "error",
                "-threads",
                "2",
                "-filter_threads",
                "1",
                "-ss",
                &format!("{seconds:.6}"),
                "-i",
            ])
            .arg(&asset.path)
            .args([
                "-map",
                &format!("0:{}", stream.index),
                "-frames:v",
                "1",
                "-vf",
                &format!(
                "scale={}:{}:force_original_aspect_ratio=decrease,pad={}:{}:(ow-iw)/2:(oh-ih)/2",
                size.0, size.1, size.0, size.1
            ),
                "-pix_fmt",
                "bgra",
                "-f",
                "rawvideo",
                "pipe:1",
            ]),
        cancelled,
    )?;
    ensure!(
        output.status.success() && output.stdout.len() == size.0 as usize * size.1 as usize * 4,
        "Could not read recording thumbnail"
    );
    Ok(crate::PreviewFrame {
        width: size.0,
        height: size.1,
        frame: 0,
        bgra: output.stdout,
    })
}

/// Stream PCM into 10 ms peak/RMS bins; never retain a whole decoded soundtrack.
pub fn waveform(path: &Path, stream: u32) -> Result<Vec<WaveBin>> {
    waveform_cancellable(path, stream, || false)
}

pub fn waveform_cancellable(
    path: &Path,
    stream: u32,
    cancelled: impl Fn() -> bool,
) -> Result<Vec<WaveBin>> {
    ensure!(!cancelled(), "Media task cancelled");
    let mut child = ToolChild(
        media_command("ffmpeg")
            .args(["-v", "error", "-threads", "2", "-i"])
            .arg(path)
            .args([
                "-map",
                &format!("0:{stream}"),
                "-vn",
                "-ac",
                "1",
                "-ar",
                "48000",
                "-f",
                "f32le",
                "pipe:1",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?,
    );
    let mut pipe = child.0.stdout.take().context("waveform pipe missing")?;
    let (tx, rx) = mpsc::sync_channel(2);
    let reader = thread::Builder::new()
        .name("rcr-waveform-pipe".into())
        .spawn(move || {
            let result = (|| -> Result<()> {
                let mut bytes = [0; 4 * 480];
                loop {
                    let mut count = 0;
                    while count < bytes.len() {
                        let got = pipe.read(&mut bytes[count..])?;
                        if got == 0 {
                            break;
                        }
                        count += got;
                    }
                    if count == 0 {
                        break;
                    }
                    ensure!(count.is_multiple_of(4), "unaligned PCM output");
                    let mut bin = WaveBin::default();
                    let mut sum = 0.;
                    for sample in bytes[..count].as_chunks::<4>().0 {
                        let value = f32::from_le_bytes(*sample);
                        ensure!(value.is_finite(), "Non-finite decoded audio sample");
                        let value = value.clamp(-1., 1.);
                        bin.min = bin.min.min(value);
                        bin.max = bin.max.max(value);
                        sum += value * value;
                    }
                    bin.rms = (sum / (count / 4) as f32).sqrt();
                    tx.send(Ok(bin))
                        .map_err(|_| anyhow::anyhow!("Waveform cancelled"))?;
                }
                Ok(())
            })();
            if let Err(error) = result {
                let _ = tx.send(Err(error));
            }
        })?;
    let result = (|| -> Result<Vec<WaveBin>> {
        let mut bins = Vec::new();
        loop {
            ensure!(!cancelled(), "Media task cancelled");
            match rx.recv_timeout(Duration::from_millis(10)) {
                Ok(bin) => {
                    bins.push(bin?);
                    ensure!(bins.len() <= 1_000_000, "waveform duration limit exceeded");
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        loop {
            ensure!(!cancelled(), "Media task cancelled");
            if let Some(status) = child.0.try_wait()? {
                ensure!(status.success(), "audio waveform decoding failed");
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        Ok(bins)
    })();
    if result.is_err() {
        let _ = child.0.kill();
        let _ = child.0.wait();
    }
    drop(rx);
    reader
        .join()
        .map_err(|_| anyhow::anyhow!("Waveform pipe reader panicked"))?;
    result
}

pub fn recordings_dir() -> PathBuf {
    PathBuf::from("recordings")
}

#[cfg(test)]
mod cancellation_tests {
    use super::*;
    #[test]
    #[ignore = "owned subprocess fixture, invoked only by the pipe supervisor test"]
    fn blocked_stdout_fixture() {
        if std::env::var_os("RCR_TEST_BLOCKED_STDOUT").is_none() {
            return;
        }
        use std::io::Write;
        std::io::stdout()
            .write_all(b"owned pipe fixture ready\n")
            .unwrap();
        std::io::stdout().flush().unwrap();
        loop {
            thread::sleep(Duration::from_secs(60));
        }
    }
    #[test]
    fn stalled_pipe_and_consumer_failure_cancel_reap_and_join_the_owned_child() {
        let command = || {
            let mut command = Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--ignored",
                    "--exact",
                    "tools::cancellation_tests::blocked_stdout_fixture",
                    "--nocapture",
                ])
                .env("RCR_TEST_BLOCKED_STDOUT", "1")
                .stdin(Stdio::null())
                .stderr(Stdio::null());
            command
        };
        let started = std::time::Instant::now();
        let error = stream_media_output(
            &mut command(),
            3840,
            || started.elapsed() >= Duration::from_millis(150),
            |_| panic!("fixture must block before a full chunk"),
        )
        .unwrap_err();
        assert!(error.to_string().contains("cancelled"));
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "Blocked pipe prevented bounded cancellation"
        );
        let started = std::time::Instant::now();
        let error = stream_media_output(
            &mut command(),
            1,
            || false,
            |_| anyhow::bail!("owned consumer failed"),
        )
        .unwrap_err();
        assert!(error.to_string().contains("owned consumer failed"));
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "Full reader queue prevented cleanup"
        );
    }
    #[test]
    fn media_pipe_reader_enforces_its_bound() {
        assert_eq!(bounded_pipe(&b"1234"[..], 4).unwrap(), b"1234");
        assert!(bounded_pipe(&b"12345"[..], 4).is_err());
    }
    #[test]
    fn cancelled_metadata_jobs_do_not_open_sources_or_start_tools() {
        let missing = Path::new("missing-cancelled-fixture.mkv");
        assert!(probe_cancellable(missing, || true)
            .unwrap_err()
            .to_string()
            .contains("cancelled"));
        assert!(waveform_cancellable(missing, 0, || true)
            .unwrap_err()
            .to_string()
            .contains("cancelled"));
    }
}
