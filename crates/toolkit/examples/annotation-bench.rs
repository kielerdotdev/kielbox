//! Synthetic desktop drag. No display, clipboard or recorder access.
use toolkit::{
    annotation as a, effects,
    image_editor::{Actor, Command, Preview},
};
use std::time::{Duration, Instant};
fn next(actor: &Actor) -> Preview {
    let limit = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(p) = actor.take_preview() {
            return p;
        }
        assert!(Instant::now() < limit, "Preview timed out");
        std::thread::sleep(Duration::from_millis(1));
    }
}
fn main() -> anyhow::Result<()> {
    let actor = Actor::desktop(effects::solid_frame(5120, 1440, [40, 70, 90, 255]))?;
    let initial = next(&actor);
    println!("initial_us={}", initial.render_us);
    actor.send(Command::Tool(a::Tool::Arrow))?;
    actor.send(Command::Down(
        a::PointF::new(100., 100.),
        a::Modifiers::NONE,
    ))?;
    while next(&actor).objects == 0 {}
    let mut times = vec![];
    let mut latency = vec![];
    let mut bytes = 0usize;
    for i in 0..35 {
        let sent = Instant::now();
        actor.send(Command::Move(
            a::PointF::new(180. + i as f32 * 3., 180. + i as f32),
            a::Modifiers::NONE,
        ))?;
        let preview = next(&actor);
        if i >= 5 {
            latency.push(sent.elapsed().as_micros());
            times.push(preview.render_us);
            bytes += preview
                .tiles
                .iter()
                .map(|tile| tile.bgra.len())
                .sum::<usize>();
        }
    }
    actor.send(Command::Up(a::PointF::new(282., 214.), a::Modifiers::NONE))?;
    times.sort();
    latency.sort();
    println!(
        "samples={}, raster_p50_us={}, raster_p95_us={}, worker_response_p95_us={}, mean_upload_bytes={}",
        times.len(),
        times[times.len() / 2],
        times[times.len() * 95 / 100],
        latency[latency.len() * 95 / 100],
        bytes / times.len()
    );
    Ok(())
}
