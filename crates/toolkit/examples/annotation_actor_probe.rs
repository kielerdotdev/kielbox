//! CPU annotation actor benchmark; no GPUI, native windows, capture, or clipboard.
//! Run: cargo run -p toolkit --example annotation_actor_probe --release -- 3
//! Prints one JSON record per measurement. Native GPU upload/presentation is excluded.
use anyhow::{Context, Result, bail, ensure};
use toolkit::{
    annotation as a,
    image_editor::{Actor, Command, Event, Preview},
    types::Frame,
};
use serde::Serialize;
use std::{sync::mpsc::RecvTimeoutError, thread, time::{Duration, Instant}};

const WIDTH: u32 = 2560;
const HEIGHT: u32 = 1440;
const DEADLINE: Duration = Duration::from_secs(60);

#[derive(Default, Serialize)]
struct Measurement {
    case: String,
    iteration: usize,
    commands_accepted: usize,
    move_events_submitted: usize,
    queue_busy_retries: usize,
    enqueue_ms: f64,
    command_to_preview_ms: f64,
    previews_consumed: usize,
    render_us_total_observed: u64,
    render_us_last: u64,
    render_us_max_observed: u64,
    dirty_tile_bytes_observed: usize,
    dirty_tiles_observed: usize,
    final_objects: usize,
}
impl Measurement {
    fn observe(&mut self, p: &Preview) {
        self.previews_consumed += 1;
        self.render_us_last = p.render_us;
        self.render_us_max_observed = self.render_us_max_observed.max(p.render_us);
        self.render_us_total_observed += p.render_us;
        self.dirty_tile_bytes_observed += p.tiles.iter().map(|t| t.bgra.len()).sum::<usize>();
        self.dirty_tiles_observed += p.tiles.len();
        self.final_objects = p.objects;
    }
}

fn check_events(actor: &Actor) -> Result<()> {
    for event in actor.events.try_iter() {
        if let Event::Error(error) = event { bail!("Annotation actor: {error}"); }
    }
    Ok(())
}
fn send(actor: &Actor, result: &mut Measurement, command: impl Fn() -> Command) -> Result<()> {
    let started = Instant::now();
    loop {
        match actor.send(command()) {
            Ok(()) => { result.commands_accepted += 1; return Ok(()); }
            Err(error) => {
                // Retrying the same event preserves Down/Move/Up order under bounded
                // queue pressure. A rejected move is never silently discarded here.
                ensure!(error.to_string().contains("busy"), "Actor send failed: {error:#}");
                result.queue_busy_retries += 1;
                if let Some(preview) = actor.take_preview() { result.observe(&preview); }
                check_events(actor)?;
                ensure!(started.elapsed() < DEADLINE, "Actor queue did not recover");
                thread::sleep(Duration::from_millis(1));
            }
        }
    }
}
fn wait_preview(actor: &Actor, result: &mut Measurement, ready: impl Fn(&Preview) -> bool) -> Result<()> {
    let started = Instant::now();
    loop {
        check_events(actor)?;
        if let Some(preview) = actor.take_preview() {
            let done = ready(&preview);
            result.observe(&preview);
            if done { return Ok(()); }
        }
        ensure!(started.elapsed() < DEADLINE, "Timed out waiting for {} preview", result.case);
        thread::sleep(Duration::from_millis(1));
    }
}
fn desktop() -> Result<Frame> {
    let mut bytes = Vec::with_capacity(WIDTH as usize * HEIGHT as usize * 4);
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let checker = ((x / 9 + y / 9) % 2) as u8;
            bytes.extend_from_slice(&[
                ((x * 3 + y) % 251) as u8,
                ((y * 5) % 239) as u8,
                55 + checker * 150,
                255,
            ]);
        }
    }
    Ok(Frame::from_rgba8(WIDTH, HEIGHT, bytes)?)
}
fn new_actor(iteration: usize) -> Result<Actor> {
    let started = Instant::now();
    let actor = Actor::desktop(desktop()?)?;
    let mut result = Measurement { case: "initial_desktop".into(), iteration, ..Default::default() };
    wait_preview(&actor, &mut result, |p| p.width == WIDTH && p.height == HEIGHT)?;
    result.command_to_preview_ms = started.elapsed().as_secs_f64() * 1000.;
    ensure!(result.dirty_tile_bytes_observed == WIDTH as usize * HEIGHT as usize * 4,
        "Initial native-size raster upload was incomplete");
    println!("{}", serde_json::to_string(&result)?);
    Ok(actor)
}

fn edit(
    actor: &Actor, iteration: usize, label: &str, tool: a::Tool,
    expected_objects: usize, samples: usize, freehand: bool,
) -> Result<Measurement> {
    let mut setup = Measurement { case: format!("{label}_tool_selection"), iteration, ..Default::default() };
    let started = Instant::now();
    send(actor, &mut setup, || Command::ClearSelection)?;
    send(actor, &mut setup, || Command::Tool(tool))?;
    wait_preview(actor, &mut setup, |p| p.properties.as_ref()
        .is_some_and(|p| p.tool == tool && p.selected == 0))?;
    setup.command_to_preview_ms = started.elapsed().as_secs_f64() * 1000.;
    println!("{}", serde_json::to_string(&setup)?);

    let mut result = Measurement { case: label.into(), iteration, move_events_submitted: samples, ..Default::default() };
    let started = Instant::now();
    let (left, top) = if freehand { (120., 880.) } else { (640., 260.) };
    let point = |i: usize| {
        if freehand {
            a::PointF::new(left + i as f32 * 1100. / samples as f32,
                top + if i.is_multiple_of(2) { 0. } else { 90. })
        } else {
            let t = (i + 1) as f32 / samples as f32;
            a::PointF::new(left + 720. * t, top + 400. * t)
        }
    };
    send(actor, &mut result, || Command::Down(a::PointF::new(left, top), a::Modifiers::NONE))?;
    for i in 0..samples {
        send(actor, &mut result, || Command::Move(point(i), a::Modifiers::NONE))?;
    }
    send(actor, &mut result, || Command::Up(point(samples - 1), a::Modifiers::NONE))?;
    result.enqueue_ms = started.elapsed().as_secs_f64() * 1000.;
    wait_preview(actor, &mut result, |p| p.objects == expected_objects
        && p.overlay.creation.is_none()
        && p.properties.as_ref().is_some_and(|p| p.selected == 1))?;
    result.command_to_preview_ms = started.elapsed().as_secs_f64() * 1000.;
    ensure!(result.final_objects == expected_objects, "Lost or duplicated completed annotation");
    Ok(result)
}

fn inspect_project(actor: &Actor, expected_objects: usize, minimum_pen_samples: usize) -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("actor-benchmark.ssxe");
    let mut probe = Measurement::default();
    send(actor, &mut probe, || Command::Project(path.clone()))?;
    let started = Instant::now();
    loop {
        match actor.events.recv_timeout(Duration::from_millis(100)) {
            Ok(Event::Saved(saved)) => { ensure!(saved == path, "Unexpected project output"); break; }
            Ok(Event::Error(error)) => bail!("Project verification failed: {error}"),
            Ok(_) | Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => bail!("Actor disconnected during verification"),
        }
        ensure!(started.elapsed() < DEADLINE, "Project verification timed out");
    }
    let document = a::project::load(&path).context("Read benchmark's temporary project")?;
    ensure!(document.objects().len() == expected_objects, "Project lost final annotations");
    let pen = document.objects().iter().find_map(|object| {
        if let a::ObjectKind::Freehand(stroke) = &object.kind { Some(stroke) } else { None }
    }).context("No completed pen stroke in project")?;
    ensure!(pen.points.len() >= minimum_pen_samples,
        "Pen burst lost samples: {} < {minimum_pen_samples}", pen.points.len());
    ensure!(pen.points.iter().any(|p| p.y == 880.) && pen.points.iter().any(|p| p.y == 970.),
        "Pen burst lost its alternating intermediate path");
    println!("{}", serde_json::json!({"verification":"project_objects_and_pen_path", "objects":expected_objects,
        "pen_points":pen.points.len(), "minimum_pen_samples":minimum_pen_samples}));
    Ok(())
}

fn main() -> Result<()> {
    let iterations = std::env::args().nth(1).map(|arg| arg.parse::<usize>()).transpose()?.unwrap_or(3);
    ensure!((1..=20).contains(&iterations), "Iterations must be 1–20");
    println!("{}", serde_json::json!({"benchmark":"annotation_actor", "canvas":[WIDTH,HEIGHT],
        "iterations":iterations, "native_gpu_upload_excluded":true,
        "notes":"Dirty bytes are BGRA CPU tiles delivered to this consumer, not measured GPU uploads. Latest-preview mailbox may merge intermediate frames; render_us_total_observed covers consumed previews only. Queue backpressure retries preserve every accepted stroke event."}));
    for iteration in 0..iterations {
        let actor = new_actor(iteration)?;
        for (index, (label, tool, samples, pen)) in [
            ("rectangle", a::Tool::Rectangle, 32, false),
            ("pen", a::Tool::Freehand, 96, true),
            ("blur", a::Tool::Blur, 32, false),
            ("pixelate", a::Tool::Pixelate, 32, false),
        ].into_iter().enumerate() {
            let result = edit(&actor, iteration, label, tool, index + 1, samples, pen)?;
            println!("{}", serde_json::to_string(&result)?);
        }
        inspect_project(&actor, 4, 96)?;
    }
    // Shape move observations may coalesce; pen samples must survive the same
    // producer burst. Both submit more than the 256-command queue capacity;
    // shape moves can coalesce before they occupy that capacity.
    let actor = new_actor(iterations)?;
    for (index, (label, tool, samples, pen)) in [
        ("rectangle_move_burst_4096", a::Tool::Rectangle, 4096, false),
        ("pen_stroke_burst_1024", a::Tool::Freehand, 1024, true),
    ].into_iter().enumerate() {
        let result = edit(&actor, iterations, label, tool, index + 1, samples, pen)?;
        println!("{}", serde_json::to_string(&result)?);
    }
    inspect_project(&actor, 2, 1024)?;
    Ok(())
}
