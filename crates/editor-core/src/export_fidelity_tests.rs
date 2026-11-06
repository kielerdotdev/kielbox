//! CPU-only golden audio checks for the actual export graph. No audio device.
use super::*;

fn floats(path: &Path) -> Result<Vec<f32>> {
    let bytes = fs::read(path)?;
    ensure!(bytes.len().is_multiple_of(4), "Incomplete float PCM");
    Ok(bytes.as_chunks::<4>().0.iter().map(|bytes| f32::from_le_bytes(*bytes)).collect())
}

fn render_audio(project: &Project, start: u64, end: u64, folder: &Path) -> Result<Vec<Vec<f32>>> {
    let mut plan = RenderPlan::build_mode(project, start, end, (16, 16, 60),
        Some(AudioExport::MixAndSeparate), false, &HashMap::new())?;
    plan.filter.push_str(";[vout]nullsink");
    let mut command = plan.command();
    let mut paths = Vec::new();
    for (index, (_, label)) in plan.audio.iter().enumerate() {
        let path = folder.join(format!("stream-{index}.f32"));
        command.args(["-map", &format!("[{label}]"), "-c:a", "pcm_f32le", "-ar", "48000",
            "-ac", "2", "-f", "f32le", "-n"]).arg(&path);
        paths.push(path);
    }
    let output = command.output()?;
    ensure!(output.status.success(), "Export graph failed: {}", String::from_utf8_lossy(&output.stderr));
    paths.iter().map(|path| floats(path)).collect()
}

#[test]
#[ignore = "requires FFmpeg/FFprobe; generates and decodes private WAV fixtures, no devices"]
fn export_audio_fidelity_matches_sample_reference() -> Result<()> {
    let folder = tempfile::tempdir()?;
    let mut project = Project { width: 16, height: 16, fps: 60, next_id: 7, ..Default::default() };
    let mut reference = Vec::new();
    let mut source_stamps = Vec::new();
    for (index, frequency) in [440, 880].iter().enumerate() {
        let path = folder.path().join(format!("tone-{frequency}.wav"));
        let expression = format!("aevalsrc=0.1*sin(2*PI*{frequency}*t)|0.1*sin(2*PI*{frequency}*t):s=48000:d=2");
        let generated = media_command("ffmpeg").args(["-v", "error", "-f", "lavfi", "-i", &expression,
            "-c:a", "pcm_f32le", "-n"]).arg(&path).output()?;
        ensure!(generated.status.success(), "Source generation failed: {}", String::from_utf8_lossy(&generated.stderr));
        let mut asset = crate::probe(&path)?;
        asset.id = index as u64 + 1;
        source_stamps.push(crate::SourceStamp::read(&path)?);
        let decoded = media_command("ffmpeg").args(["-v", "error", "-i"]).arg(&path)
            .args(["-map", "0:a:0", "-f", "f32le", "pipe:1"]).output()?;
        ensure!(decoded.status.success(), "Reference decode failed");
        let pcm = decoded.stdout.as_chunks::<4>().0.iter().map(|bytes| f32::from_le_bytes(*bytes)).collect::<Vec<_>>();
        ensure!(pcm.len() == 96_000 * 2, "Unexpected source sample count");
        reference.push(pcm);
        let track = index as u64 + 3;
        project.tracks.push(crate::Track { id: track, name: format!("Tone {frequency}"), kind: TrackKind::Audio,
            muted: false, solo: false, gain: 1., suppress_noise: false });
        project.clips.push(Clip { id: index as u64 + 5, track, group: 1,
            content: Content::Media { asset: asset.id, stream: 0 }, start: 0, duration: 120,
            source_in_ns: 0, speed: 1., gain: 1., fade_in: 0, fade_out: 0,
            transform: Transform::default(), keyframes: vec![] });
        project.assets.push(asset);
    }
    for case in ["full", "trim_gain", "mute", "solo", "trim_fade"] {
        let mut edited = project.clone();
        let (start, end) = match case { "trim_gain" => (30, 90), "trim_fade" => (15, 45), _ => (0, 120) };
        let mut gains = [1_f32, 1.];
        if case == "trim_gain" {
            edited.tracks[0].gain = 0.5; edited.clips[0].gain = 0.5; gains[0] = 0.25;
            edited.tracks[1].gain = 0.75; edited.clips[1].gain = 0.5; gains[1] = 0.375;
        }
        if case == "mute" { edited.tracks[0].muted = true; }
        if case == "solo" { edited.tracks[1].solo = true; }
        if case == "trim_fade" { for clip in &mut edited.clips { clip.fade_in = 30; } }
        let selected = if matches!(case, "mute" | "solo") { vec![1] } else { vec![0, 1] };
        let output_folder = folder.path().join(case);
        fs::create_dir(&output_folder)?;
        let rendered = render_audio(&edited, start, end, &output_folder)?;
        ensure!(rendered.len() == selected.len() + 1, "Wrong mix/stem count in {case}");
        let first = start as usize * 800;
        let count = (end - start) as usize * 800;
        let mut expected_mix = vec![0_f32; count * 2];
        let mut maximum_error = 0_f32;
        for (stem, index) in selected.iter().enumerate() {
            ensure!(rendered[stem + 1].len() == count * 2, "Wrong stem sample count in {case}");
            for sample in 0..count {
                let envelope = if case == "trim_fade" { ((first + sample) as f32 / 24_000.).min(1.) } else { 1. };
                for channel in 0..2 {
                    let at = sample * 2 + channel;
                    let expected = reference[*index][(first + sample) * 2 + channel] * gains[*index] * envelope;
                    maximum_error = maximum_error.max((rendered[stem + 1][at] - expected).abs());
                    expected_mix[at] += expected;
                }
            }
        }
        ensure!(rendered[0].len() == expected_mix.len(), "Wrong mixed sample count in {case}");
        maximum_error = rendered[0].iter().zip(&expected_mix)
            .fold(maximum_error, |error, (actual, expected)| error.max((actual - expected).abs()));
        if maximum_error > 0.000_02 {
            let (sample, (actual, expected)) = rendered[0].iter().zip(&expected_mix).enumerate()
                .max_by(|(_, (a, b)), (_, (c, d))| (*a - *b).abs().total_cmp(&(*c - *d).abs())).unwrap();
            eprintln!("Export audio difference: {}", serde_json::json!({"case": case,
                "stereo_frame": sample / 2, "actual": actual, "expected": expected,
                "last_nonzero_stereo_frame": rendered[0].iter().rposition(|value| value.abs() > 0.0001).map(|index| index / 2),
                "expected_frames": count, "maximum_error": maximum_error}));
        }
        ensure!(maximum_error <= 0.000_02, "Export audio fidelity failed in {case}: maximum error {maximum_error}");
        println!("Export audio fidelity JSON: {}", serde_json::json!({"case": case, "frames_48khz": count,
            "stems": selected.len(), "maximum_sample_error": maximum_error,
            "scope": "Actual export filter graph versus independently decoded PCM/sample envelope; no GPU or audio device"}));
    }
    for (asset, before) in project.assets.iter().zip(source_stamps) {
        ensure!(crate::SourceStamp::read(&asset.path)? == before, "Proof changed its source");
    }
    Ok(())
}

#[test]
#[ignore = "requires installed FFmpeg/FFprobe and H.264 NVENC; uses generated files, no capture or audio devices"]
fn share_h264_export_matches_lossless_graph_and_preserves_sources() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let source = directory.path().join("source.mkv");
    let generated = media_command("ffmpeg").args(["-v", "error", "-f", "lavfi", "-i",
        "testsrc2=size=512x288:rate=30:duration=2", "-f", "lavfi", "-i",
        "sine=frequency=440:sample_rate=48000:duration=2", "-c:v", "libx264", "-preset", "veryfast",
        "-threads", "2", "-pix_fmt", "yuv420p", "-c:a", "pcm_f32le", "-ac", "2", "-n"])
        .arg(&source).output()?;
    ensure!(generated.status.success(), "Could not generate source: {}", String::from_utf8_lossy(&generated.stderr));
    let before = crate::SourceStamp::read(&source)?;
    let mut editor = crate::Editor::default();
    editor.execute(crate::Edit::Sequence(crate::QuickEdit::Add(crate::probe(&source)?)))?;
    let project_before = serde_json::to_value(&editor.project)?;
    let destination = directory.path().join("share.mp4");
    let options = ExportOptions { output: destination.clone(), start: 30, end: Some(90),
        audio: AudioExport::Mix, bitrate: 8_000_000, video_codec: ExportVideoCodec::H264 };
    let progress = JobProgress::default();
    let exported = export(&editor.project, &options, &progress)?;
    ensure!(progress.done.load(Ordering::Acquire), "Export completion was not acknowledged");
    let metadata = media_command("ffprobe").args(["-v", "error", "-count_frames", "-show_streams", "-show_format", "-of", "json"])
        .arg(&exported).output()?;
    ensure!(metadata.status.success(), "Could not inspect exported file");
    let metadata: serde_json::Value = serde_json::from_slice(&metadata.stdout)?;
    let streams = metadata["streams"].as_array().context("Missing exported streams")?;
    let video = streams.iter().find(|stream| stream["codec_type"] == "video").context("Missing video")?;
    let audio = streams.iter().find(|stream| stream["codec_type"] == "audio").context("Missing mixed audio")?;
    ensure!(video["codec_name"] == "h264" && audio["codec_name"] == "aac", "Sharing codecs changed");
    ensure!(video["width"] == 512 && video["height"] == 288 && video["r_frame_rate"] == "60/1"
        && video["nb_read_frames"] == "60", "Export trim, dimensions or frame count changed: {video}");
    ensure!(audio["sample_rate"] == "48000" && audio["channels"] == 2, "Sharing audio format changed");
    let seconds: f64 = metadata["format"]["duration"].as_str().context("No duration")?.parse()?;
    ensure!((seconds - 1.).abs() < 0.03, "Export range duration changed: {seconds}");
    let reference = directory.path().join("reference.mkv");
    let plan = RenderPlan::build(&editor.project, 30, 90, 512, 288, 60, None)?;
    let lossless = plan.command().args(["-map", "[vout]", "-an", "-c:v", "ffv1", "-threads", "2", "-n"])
        .arg(&reference).output()?;
    ensure!(lossless.status.success(), "Lossless graph reference failed: {}", String::from_utf8_lossy(&lossless.stderr));
    let compared = media_command("ffmpeg").args(["-hide_banner", "-threads", "2", "-i"]).arg(&exported)
        .args(["-threads", "2", "-i"]).arg(&reference)
        .args(["-filter_complex_threads", "2", "-lavfi", "[0:v][1:v]ssim", "-an", "-f", "null", "-"]).output()?;
    ensure!(compared.status.success(), "Export/reference full decode failed");
    let stderr = String::from_utf8_lossy(&compared.stderr);
    let ssim: f64 = stderr.lines().filter_map(|line| line.split_once("All:").map(|(_, tail)| tail))
        .last().context("No SSIM result")?.split_whitespace().next().context("No SSIM value")?.parse()?;
    ensure!(ssim >= 0.98, "Export content differs from its trimmed graph: SSIM {ssim}");
    let previous = fs::read(&destination)?;
    ensure!(export(&editor.project, &options, &JobProgress::default()).is_err(), "Existing output was overwritten");
    ensure!(fs::read(&destination)? == previous, "Collision changed the completed export");
    ensure!(crate::SourceStamp::read(&source)? == before && serde_json::to_value(&editor.project)? == project_before,
        "Export changed its source or project");
    println!("Share H264 export JSON: {}", serde_json::json!({"codec": "h264", "audio": "aac",
        "frames": 60, "duration_seconds": seconds, "ssim_vs_lossless_graph": ssim,
        "source_and_project_unchanged": true, "existing_output_preserved": true,
        "scope": "Actual H264 NVENC export, generated source, independent full decode and lossless trimmed graph; no capture/audio device"}));
    Ok(())
}
