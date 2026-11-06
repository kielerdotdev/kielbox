//! Local integration fixtures. No screenshots, clipboard access or network traffic.
use toolkit::{
    annotation as a,
    core::workflow::CancelToken,
    effects,
    utilities::{Request, Tool, run, save_image},
};
fn request(tool: Tool) -> Request {
    Request {
        tool,
        files: vec![],
        text: String::new(),
        mode: 0,
        a: 32,
        b: 16,
        color: "#20352D".into(),
        output: None,
    }
}
fn main() -> anyhow::Result<()> {
    toolkit::utilities::with_pool(proof)?
}
fn proof() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let image = root.path().join("input.png");
    let mut session = a::EditorSession::from_frame(effects::solid_frame(640, 180, [255; 4]))?;
    session.set_tool(a::Tool::Text);
    session.pointer_down(a::PointF::new(20., 20.), a::Modifiers::NONE, None);
    session.pointer_up(a::PointF::new(580., 140.), a::Modifiers::NONE);
    session.text_insert("RCR TEST 12345");
    session.commit_text_edit();
    save_image(&image, &session.render(&Default::default()))?;
    for tool in [
        Tool::Viewer,
        Tool::Beautify,
        Tool::Thumbnail,
        Tool::Convert,
        Tool::Effects,
        Tool::Combine,
    ] {
        let mut r = request(tool);
        r.files = vec![image.clone()];
        if tool == Tool::Combine {
            r.files.push(image.clone());
        }
        if tool == Tool::Thumbnail {
            r.a = 160;
            r.b = 90;
        }
        let out = run(r, &CancelToken::new())?;
        anyhow::ensure!(
            out.preview.is_some() && out.image.is_some(),
            "No image from {tool:?}"
        );
        println!("{}: {}", tool.name(), out.text);
    }
    let mut r = request(Tool::Ocr);
    r.files = vec![image];
    match run(r, &CancelToken::new()) {
        Ok(out) => {
            anyhow::ensure!(
                out.text.contains("RCR") && out.text.contains("12345"),
                "OCR fixture mismatch: {}",
                out.text
            );
            println!("OCR: {}", out.text);
        }
        Err(e) => println!("OCR unavailable on this host: {e:#}"),
    };
    if let Some(input) = std::env::args_os().nth(1) {
        let original = std::path::PathBuf::from(input).canonicalize()?;
        let input = root.path().join("short-fixture.mkv");
        let args = vec![
            "-hide_banner".into(),
            "-loglevel".into(),
            "error".into(),
            "-nostdin".into(),
            "-n".into(),
            "-i".into(),
            original.to_string_lossy().into_owned(),
            "-t".into(),
            "2".into(),
            "-map".into(),
            "0:v:0".into(),
            "-map".into(),
            "0:a?".into(),
            "-c".into(),
            "copy".into(),
            input.to_string_lossy().into_owned(),
        ];
        let mut spec = toolkit::services::command::ProcessSpec::new("ffmpeg", &args);
        spec.capture_stderr = true;
        let result = toolkit::services::command::run_process(&spec, &CancelToken::new())?;
        anyhow::ensure!(result.success, "Fixture trim failed: {}", result.stderr);
        for mode in 0..3 {
            let mut r = request(Tool::VideoConvert);
            r.files = vec![input.clone()];
            r.mode = mode;
            r.a = 8;
            r.b = 320;
            let ext = ["mp4", "webm", "gif"][mode];
            r.output = Some(root.path().join(format!("converted.{ext}")));
            let out = run(r, &CancelToken::new())?;
            let args = vec![
                "-v".into(),
                "error".into(),
                "-show_entries".into(),
                "stream=codec_name,codec_type,width,height".into(),
                "-of".into(),
                "json".into(),
                out.files[0].to_string_lossy().into_owned(),
            ];
            let mut spec = toolkit::services::command::ProcessSpec::new("ffprobe", &args);
            spec.capture_stdout = true;
            spec.capture_stderr = true;
            let info = toolkit::services::command::run_process(&spec, &CancelToken::new())?;
            anyhow::ensure!(info.success, "ffprobe failed");
            let v: serde_json::Value = serde_json::from_slice(&info.stdout)?;
            let streams = v["streams"].as_array().unwrap();
            let audio = streams
                .iter()
                .filter(|s| s["codec_type"] == "audio")
                .count();
            anyhow::ensure!(
                streams.iter().any(|s| s["codec_type"] == "video"),
                "Missing video"
            );
            if mode < 2 {
                anyhow::ensure!(audio == 2, "Separate audio tracks were lost");
            }
            println!(
                "Video {ext}: {} streams / {audio} audio tracks",
                streams.len()
            );
        }
        let mut r = request(Tool::VideoThumbnail);
        r.files = vec![input];
        r.a = 1;
        r.b = 320;
        let out = run(r, &CancelToken::new())?;
        anyhow::ensure!(out.image.is_some());
        println!("Video thumbnail: {}", out.text);
    }
    Ok(())
}
