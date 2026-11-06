//! Hidden, generated-content GPUI fixture. No desktop input, capture or audio devices.
use super::*;
use anyhow::{Context as _, Result, ensure};
use ::toolkit::types::{Frame, Monitor, Rect, WindowInfo};
use std::{
    cell::RefCell,
    collections::BTreeMap,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

pub(crate) type Probe = Rc<RefCell<BTreeMap<&'static str, Bounds<Pixels>>>>;
pub(crate) fn measure(probe: Probe, key: &'static str) -> Div {
    div().absolute().top_0().left_0().size_full().child(
        canvas(
            |_, _, _| (),
            move |bounds, _, _, _| {
                probe.borrow_mut().insert(key, bounds);
            },
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full(),
    )
}
fn generated_worker(options: Options) -> Result<Worker> {
    use recording_tools::{
        audio::synth::{Signal, SyntheticAudio, SyntheticAudioConfig},
        source::synthetic::{SyntheticConfig, SyntheticSource, TimeMode},
    };
    Worker::start_with_sources(
        options,
        Box::new(|options, fps| {
            let video = SyntheticSource::new(SyntheticConfig::new(400, 240, fps));
            let audio = [options.system_audio, options.microphone]
                .into_iter()
                .enumerate()
                .filter(|(_, enabled)| *enabled)
                .map(|(index, _)| {
                    Box::new(SyntheticAudio::new(SyntheticAudioConfig::new(
                        Signal::Tone {
                            hz: if index == 0 { 440. } else { 880. },
                            amplitude: 0.2,
                            start: Duration::ZERO,
                        },
                        TimeMode::Realtime,
                    ))) as Box<dyn recording_tools::audio::AudioSource>
                })
                .collect();
            Ok((Box::new(video), audio))
        }),
    )
}

fn desktop(_: &CancelToken) -> Result<picker::Desktop> {
    let mut frame = Frame::from_rgba8(1800, 1200, vec![0; 1800 * 1200 * 4])?;
    frame.origin = ::toolkit::types::Point::new(-1800, -100);
    frame.scale_factor = 2.;
    for (index, pixel) in frame
        .data_mut()
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .enumerate()
    {
        let (x, y) = (index % 1800, index / 1800);
        let color = if (100..650).contains(&x) && (140..540).contains(&y) {
            if y < 180 {
                [55, 65, 84, 255]
            } else {
                [31, 40, 58, 255]
            }
        } else if (1000..1680).contains(&x) && (200..720).contains(&y) {
            if y < 240 {
                [88, 71, 70, 255]
            } else {
                [48, 40, 47, 255]
            }
        } else {
            [20 + (x / 40) as u8, 39 + (y / 28) as u8, 74, 255]
        };
        pixel.copy_from_slice(&color);
    }
    let mut with_cursor = frame.clone();
    // A deterministic pointer marker proves which frozen image was selected.
    with_cursor.data_mut()[0..4].copy_from_slice(&[255, 255, 255, 255]);
    Ok(picker::Desktop {
        frame,
        with_cursor,
        monitors: vec![
            Monitor {
                id: "left".into(),
                name: "Left test display".into(),
                rect: Rect::new(-1800, -100, 900, 1200),
                scale_factor: 2.,
                primary: false,
                refresh_hz: Some(60.),
                hdr: None,
            },
            Monitor {
                id: "right".into(),
                name: "Right test display".into(),
                rect: Rect::new(-900, -100, 900, 1200),
                scale_factor: 2.,
                primary: true,
                refresh_hz: Some(60.),
                hdr: None,
            },
        ],
        windows: vec![
            WindowInfo {
                id: "front".into(),
                title: "Generated editor".into(),
                app_name: Some("Fixture".into()),
                rect: Rect::new(-1700, 40, 550, 400),
                minimized: false,
                focused: true,
            },
            WindowInfo {
                id: "back".into(),
                title: "Generated notes".into(),
                app_name: Some("Fixture".into()),
                rect: Rect::new(-800, 100, 680, 520),
                minimized: false,
                focused: false,
            },
        ],
    })
}
fn inspect<R>(
    host: AnyWindowHandle,
    cx: &mut AsyncApp,
    read: impl FnOnce(&crate::Studio, &QuickRecorder, &App) -> R,
) -> Result<R> {
    host.update(cx, |root, _, cx| {
        let studio = root.downcast::<crate::Studio>().unwrap();
        let studio = studio.read(cx);
        read(studio, studio.quick_recorder.read(cx), cx)
    })
}
fn click(window: AnyWindowHandle, probe: &Probe, key: &str, cx: &mut AsyncApp) -> Result<()> {
    let bounds = window.update(cx, |_, window, cx| -> Result<_> {
        probe.borrow_mut().clear();
        window.refresh();
        window.draw(cx).clear();
        let b = *probe
            .borrow()
            .get(key)
            .with_context(|| format!("Missing painted {key}"))?;
        ensure!(
            b.left() >= px(0.)
                && b.top() >= px(0.)
                && b.right() <= window.viewport_size().width
                && b.bottom() <= window.viewport_size().height,
            "{key} escaped the viewport: {b:?}"
        );
        Ok(b)
    })??;
    pointer(window, bounds.center(), bounds.center(), cx)
}
fn pointer(
    window: AnyWindowHandle,
    from: Point<Pixels>,
    to: Point<Pixels>,
    cx: &mut AsyncApp,
) -> Result<()> {
    window.update(cx, |_, window, cx| {
        window.dispatch_event(
            PlatformInput::MouseDown(MouseDownEvent {
                position: from,
                button: MouseButton::Left,
                click_count: 1,
                first_mouse: false,
                modifiers: Default::default(),
            }),
            cx,
        );
        window.dispatch_event(
            PlatformInput::MouseMove(MouseMoveEvent {
                position: to,
                pressed_button: Some(MouseButton::Left),
                modifiers: Default::default(),
            }),
            cx,
        );
        window.dispatch_event(
            PlatformInput::MouseUp(MouseUpEvent {
                position: to,
                button: MouseButton::Left,
                click_count: 1,
                modifiers: Default::default(),
            }),
            cx,
        );
    })
}
fn check_alignment(window: AnyWindowHandle, probe: &Probe, cx: &mut AsyncApp) -> Result<()> {
    window.update(cx, |_, window, cx| -> Result<()> {
        probe.borrow_mut().clear();
        window.refresh(); window.draw(cx).clear();
        let measured = probe.borrow();
        let bounds = |key: &str| measured.get(key).copied().with_context(|| format!("Missing alignment bound: {key}"));
        let near = |a: Pixels, b: Pixels, name: &str| -> Result<()> {
            ensure!((f32::from(a-b)).abs() <= 1.1, "Misaligned {name}: {a:?} versus {b:?}"); Ok(())
        };
        let mut labels = Vec::new();
        for (tile, glyph, label, group) in [
            ("region", "region-glyph", "region-label", "mode-group"),
            ("screen", "screen-glyph", "screen-label", "shutter-group"),
            ("window", "window-glyph", "window-label", "utility-group"),
        ] {
            let (tile, glyph, label, group) = (bounds(tile)?, bounds(glyph)?, bounds(label)?, bounds(group)?);
            near(tile.center().x, glyph.center().x, "target icon horizontal center")?;
            near(tile.center().x, label.center().x, "target caption horizontal center")?;
            near(glyph.top()-tile.top(), tile.bottom()-label.bottom(), "target stack vertical centering")?;
            ensure!(glyph.top()-tile.top() >= px(10.) && tile.bottom()-label.bottom() >= px(10.), "Target contents touch the tile edge");
            near(label.top()-glyph.bottom(), px(4.), "icon/caption spacing")?;
            near(tile.center().x, group.center().x, "matching target/action columns")?;
            labels.push(label);
        }
        near(labels[0].bottom(), labels[1].bottom(), "Selection/Screen caption baseline")?;
        near(labels[1].bottom(), labels[2].bottom(), "Screen/Window caption baseline")?;
        let shutter = bounds("start")?;
        let fill = bounds("shutter-fill")?;
        near(shutter.center().x, fill.center().x, "shutter disk horizontal center")?;
        near(shutter.center().y, fill.center().y, "shutter disk vertical center")?;
        for (button, glyph) in [("mode-screenshot","photo-glyph"),("mode-record","video-glyph"),("cursor","pointer-glyph")] {
            let (button,glyph)=(bounds(button)?,bounds(glyph)?);
            near(button.center().x,glyph.center().x,"icon button horizontal center")?;
            near(button.center().y,glyph.center().y,"icon button vertical center")?;
            near(button.center().y,shutter.center().y,"action row center line")?;
        }
        let photo=bounds("mode-screenshot")?; let video=bounds("mode-record")?;
        near(video.left()-photo.right(),px(8.),"mode button spacing")?;
        if measured.contains_key("more") {
            let (button,glyph)=(bounds("more")?,bounds("more-glyph")?);
            near(button.center().x,glyph.center().x,"More icon horizontal center")?;
            near(button.center().y,glyph.center().y,"More icon vertical center")?;
            near(button.left()-bounds("cursor")?.right(),px(8.),"utility button spacing")?;
        }
        if measured.contains_key("audio") {
            let (audio,mic)=(bounds("audio")?,bounds("mic")?);
            near(audio.size.width,mic.size.width,"audio chip widths")?;
            near(audio.left(),bounds("region")?.left(),"audio row left edge")?;
            near(mic.right(),bounds("window")?.right(),"audio row right edge")?;
            near(mic.left()-audio.right(),px(8.),"audio chip spacing")?;
            for (cell,glyph,label) in [(audio,bounds("audio-glyph")?,bounds("audio-label")?),(mic,bounds("mic-glyph")?,bounds("mic-label")?)] {
                near(cell.center().y,glyph.center().y,"audio icon vertical center")?;
                near(cell.center().y,label.center().y,"audio label baseline")?;
                near(label.left()-glyph.right(),px(8.),"audio icon/label spacing")?;
                near((glyph.left()+label.right())*0.5,cell.center().x,"audio content horizontal center")?;
            }
        }
        Ok(())
    })?
}
fn key(window: AnyWindowHandle, key: &str, cx: &mut AsyncApp) -> Result<()> {
    window.update(cx, |_, window, cx| {
        window.dispatch_event(
            PlatformInput::KeyDown(KeyDownEvent {
                keystroke: Keystroke::parse(key).unwrap(),
                is_held: false,
            }),
            cx,
        );
    })
}
async fn resize(window: AnyWindowHandle, width: u32, height: u32, cx: &mut AsyncApp) -> Result<()> {
    let requested = size(px(width as f32), px(height as f32));
    window.update(cx, |_, window, _| window.resize(requested))?;
    let until = Instant::now() + Duration::from_secs(3);
    loop {
        cx.background_executor()
            .timer(Duration::from_millis(16))
            .await;
        if window.update(cx, |_, window, cx| {
            let ready = window.viewport_size() == requested;
            if ready {
                window.refresh();
                window.draw(cx).clear();
            }
            ready
        })? {
            return Ok(());
        }
        ensure!(Instant::now() < until, "Native resize timed out");
    }
}
async fn snapshot(window: AnyWindowHandle, path: PathBuf, cx: &mut AsyncApp) -> Result<()> {
    let done = window.update(cx, |_, window, cx| {
        window.refresh();
        window.draw(cx).clear();
        window.request_render_snapshot(path)
    })??;
    done.await??;
    Ok(())
}
async fn wait(
    host: AnyWindowHandle,
    cx: &mut AsyncApp,
    label: &str,
    ready: impl Fn(&QuickRecorder) -> bool,
) -> Result<()> {
    let until = Instant::now() + Duration::from_secs(20);
    loop {
        if inspect(host, cx, |_, r, _| ready(r))? {
            return Ok(());
        }
        ensure!(
            Instant::now() < until,
            "Timed out {label}: {}",
            inspect(host, cx, |_, r, _| r.notice.clone())?
        );
        cx.background_executor()
            .timer(Duration::from_millis(25))
            .await;
    }
}
async fn open_picker(
    host: AnyWindowHandle,
    probe: &Probe,
    video: bool,
    cx: &mut AsyncApp,
) -> Result<AnyWindowHandle> {
    click(
        host,
        probe,
        if video {
            "open-video-picker"
        } else {
            "open-picker"
        },
        cx,
    )?;
    wait(host, cx, "opening shared desktop picker", |r| {
        r.picker_window.is_some()
    })
    .await?;
    inspect(host, cx, |_, r, _| r.picker_window.unwrap())
}
pub(crate) fn run(output: PathBuf, failed: Arc<AtomicBool>, cx: &mut App) -> Result<()> {
    std::fs::create_dir_all(&output)?;
    let probe = Probe::default();
    let studio = cx.new(|cx| {
        let mut s = crate::Studio::new(None, false, true, cx);
        s.tab = 4;
        s
    });
    let toolkit = studio.read(cx).toolkit.clone();
    toolkit.update(cx, |t, cx| t.capture_menu_proof(probe.clone(), cx));
    let recorder = studio.read(cx).quick_recorder.clone();
    recorder.update(cx, |r, _| {
        r.fixture = true;
        r.snapshot_factory = desktop;
        r.worker_factory = generated_worker;
    });
    recordings::set_root(output.join("media"));
    std::fs::create_dir_all(recordings::root())?;
    let host: AnyWindowHandle = cx
        .open_window(
            WindowOptions {
                show: false,
                focus: false,
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    point(px(0.), px(0.)),
                    size(px(1440.), px(900.)),
                ))),
                ..Default::default()
            },
            |_, _| studio,
        )?
        .into();
    cx.spawn(async move |cx| {
        let result:Result<()>=async {
            let until=Instant::now()+Duration::from_secs(10);
            while !inspect(host,cx,|s,_,cx|s.toolkit.read(cx).capture_menu_ready())? { ensure!(Instant::now()<until,"Studio startup timed out"); cx.background_executor().timer(Duration::from_millis(25)).await; }
            for (width,height) in [(1440,900),(1100,680),(900,600)] {
                resize(host,width,height,cx).await?;
                snapshot(host,output.join(format!("studio-capture-{width}.png")),cx).await?;
            }
            let overlay=open_picker(host,&probe,false,cx).await?;
            for (width,height) in [(1440,900),(1100,680),(900,600)] {
                resize(overlay,width,height,cx).await?;
                snapshot(overlay,output.join(format!("desktop-screenshot-{width}.png")),cx).await?;
                check_alignment(overlay,&probe,cx)?;
                let photo_shutter = *probe.borrow().get("start").unwrap();
                click(overlay,&probe,"mode-record",cx)?;
                snapshot(overlay,output.join(format!("desktop-video-{width}.png")),cx).await?;
                check_alignment(overlay,&probe,cx)?;
                let video_shutter = *probe.borrow().get("start").unwrap();
                ensure!(f32::from(photo_shutter.center().x-video_shutter.center().x).abs() <= 1.1
                    && f32::from(photo_shutter.center().y-video_shutter.center().y).abs() <= 1.1,
                    "Switching modes moved the capture button");
                click(overlay,&probe,"mode-screenshot",cx)?;
                snapshot(overlay,output.join(format!("desktop-after-toggle-{width}.png")),cx).await?;
                overlay.update(cx,|_,w,cx|->Result<()> { probe.borrow_mut().clear(); w.refresh(); w.draw(cx).clear();
                    let toolbar=*probe.borrow().get("toolbar").context("Toolbar not measured")?;
                    ensure!(toolbar.size.width<=px(440.) && toolbar.bottom()<=w.viewport_size().height,"Toolbar escaped its container");
                    for (name,b) in probe.borrow().iter() { ensure!(b.left()>=px(0.) && b.right()<=w.viewport_size().width && b.top()>=px(0.) && b.bottom()<=w.viewport_size().height,"{name} escaped viewport"); }
                    Ok(())
                })??;
            }
            // Drawing, moving and resizing on a 2×, negative-origin desktop.
            pointer(overlay,point(px(10.),px(80.)),point(px(300.),px(300.)),cx)?;
            ensure!(inspect(host,cx,|_,r,_|r.selection.is_some() && r.worker.is_none())?,"Drag release started capture");
            pointer(overlay,point(px(100.),px(150.)),point(px(130.),px(170.)),cx)?;
            pointer(overlay,point(px(330.),px(320.)),point(px(350.),px(340.)),cx)?;
            let rect=overlay.update(cx,|root,_,cx|root.downcast::<picker::Picker>().unwrap().read(cx).selected_rect())?;
            ensure!(rect==Some(Rect::new(-1720,100,620,480)),"DPI mapping/move/resize changed: {rect:?}");
            click(overlay,&probe,"start",cx)?;
            wait(host,cx,"still picker result",|r|r.selection.is_none()).await?;
            inspect(host,cx,|s,_,cx|->Result<()> { let t=s.toolkit.read(cx); let (target,options)=t.capture_requests().last().context("Missing still request")?;
                ensure!(*target==StillTarget::Region,"Wrong still target");
                let captured=options.captured.as_ref().context("Still was recaptured instead of frozen")?;
                ensure!(captured.frame.rect()==rect.unwrap(),"Frozen crop lost its physical origin");
                let expected=desktop(&CancelToken::new())?.frame.crop_desktop(rect.unwrap())?;
                ensure!(captured.frame==expected,"Frozen screenshot pixels changed"); Ok(())
            })??;
            for target in ["screen","window"] {
                let overlay=open_picker(host,&probe,false,cx).await?;
                resize(overlay,900,600,cx).await?;
                click(overlay,&probe,target,cx)?; click(overlay,&probe,"start",cx)?;
                wait(host,cx,"explicit still result",|r|r.selection.is_none()).await?;
                inspect(host,cx,|s,_,cx|->Result<()> { let t=s.toolkit.read(cx); let (_,options)=t.capture_requests().last().unwrap();
                    if target=="screen" { let c=options.captured.as_ref().unwrap(); ensure!(c.frame.rect()==Rect::new(-900,-100,900,1200),"Wrong monitor"); }
                    else { ensure!(options.captured.is_none() && options.explicit==Some(::toolkit::services::ExplicitTarget::Window("front".into())),"Window capture must use the native window ID, not an occluded desktop crop"); } Ok(())
                })??;
            }
            // Cross-display video is rejected in the same overlay, with no worker.
            let overlay=open_picker(host,&probe,true,cx).await?; resize(overlay,900,600,cx).await?;
            pointer(overlay,point(px(10.),px(80.)),point(px(850.),px(350.)),cx)?;
            click(overlay,&probe,"start",cx)?;
            ensure!(inspect(host,cx,|_,r,_|r.selection.is_some() && r.worker.is_none())?,"Cross-display video was admitted");
            key(overlay,"escape",cx)?; wait(host,cx,"escape",|r|!r.busy()).await?;
            let overlay=open_picker(host,&probe,true,cx).await?;
            click(overlay,&probe,"more",cx)?; click(overlay,&probe,"countdown",cx)?; click(overlay,&probe,"start",cx)?; click(overlay,&probe,"cancel",cx)?;
            wait(host,cx,"countdown cancellation",|r|!r.busy()).await?;
            let overlay=open_picker(host,&probe,true,cx).await?;
            click(overlay,&probe,"audio",cx)?; click(overlay,&probe,"mic",cx)?; click(overlay,&probe,"more",cx)?; click(overlay,&probe,"fps60",cx)?;
            check_alignment(overlay,&probe,cx)?;
            click(overlay,&probe,"start",cx)?; wait(host,cx,"generated recording",|r|r.recording && r.stats.encoded>=15).await?;
            click(host,&probe,"pause",cx)?; cx.background_executor().timer(Duration::from_millis(350)).await;
            let paused=inspect(host,cx,|_,r,_|r.stats.recorded)?; cx.background_executor().timer(Duration::from_millis(350)).await;
            ensure!(inspect(host,cx,|_,r,_|r.stats.recorded==paused)?,"Paused clock advanced");
            click(host,&probe,"pause",cx)?; wait(host,cx,"resume",|r|r.stats.recorded>paused+Duration::from_millis(200)).await?;
            let overlay=open_picker(host,&probe,false,cx).await?;
            key(overlay,"v",cx)?; key(overlay,"w",cx)?; key(overlay,"p",cx)?; click(overlay,&probe,"start",cx)?;
            wait(host,cx,"concurrent still",|r|r.selection.is_none()).await?;
            ensure!(inspect(host,cx,|_,r,_|r.recording && r.worker.is_some())?,"Still capture disturbed the screencast");
            click(host,&probe,"stop",cx)?; wait(host,cx,"save",|r|!r.busy() && r.saved.is_some()).await?;
            let saved=inspect(host,cx,|_,r,_|r.saved.clone().unwrap())?; let original=std::fs::read(&saved)?;
            let overlay=open_picker(host,&probe,true,cx).await?; click(overlay,&probe,"start",cx)?;
            wait(host,cx,"second clip",|r|r.recording && r.stats.encoded>=10).await?; click(host,&probe,"cancel",cx)?;
            wait(host,cx,"discard",|r|!r.busy()).await?;
            ensure!(std::fs::read(&saved)?==original && std::fs::read_dir(saved.parent().unwrap())?.count()==1,"Discard damaged the prior clip or left staging");
            for _ in 0..6 { let overlay=open_picker(host,&probe,false,cx).await?; key(overlay,"v",cx)?;key(overlay,"v",cx)?;key(overlay,"s",cx)?;key(overlay,"escape",cx)?; wait(host,cx,"repeated close",|r|!r.busy()).await?; }
            ensure!(inspect(host,cx,|_,_,cx|cx.windows().len()==1)?,"Picker windows leaked");
            println!("Capture proof passed: actual Studio at 3 sizes; desktop photo/video overlay at 3 sizes; frozen crop exact pixels; frozen monitor and native window IDs; 2x negative-origin move/resize; drag waits for shutter; cross-display rejection; countdown cancel; audio/FPS; generated recording pause/resume; simultaneous still; save/discard; keyboard and 6 reopen/close cycles; no screen/audio devices or Computer Use");
            Ok(())
        }.await;
        if let Err(error)=result { eprintln!("Capture proof failed: {error:#}"); failed.store(true,Ordering::Relaxed); }
        let _=cx.update(|cx|cx.quit());
    }).detach();
    Ok(())
}
