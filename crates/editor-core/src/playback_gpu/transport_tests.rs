use super::*;
use crate::{Clip, MediaAsset, MediaStream, Track};

fn plain_project() -> Project {
    Project {
        assets: vec![MediaAsset {
            id: 2,
            path: PathBuf::from("never-opened.mkv"),
            source_stamp: None,
            duration_ns: 2_000_000_000,
            width: 2560,
            height: 1440,
            streams: vec![MediaStream {
                index: 0,
                kind: TrackKind::Video,
                name: "Video".into(),
            }],
        }],
        tracks: vec![Track {
            id: 1,
            name: "Video".into(),
            kind: TrackKind::Video,
            muted: false,
            solo: false,
            gain: 1.,
            suppress_noise: false,
        }],
        clips: vec![Clip {
            id: 3,
            track: 1,
            group: 3,
            content: Content::Media {
                asset: 2,
                stream: 0,
            },
            start: 0,
            duration: 120,
            source_in_ns: 0,
            speed: 1.,
            gain: 1.,
            fade_in: 0,
            fade_out: 0,
            transform: Transform::default(),
            keyframes: vec![],
        }],
        next_id: 4,
        ..Project::default()
    }
}
#[test]
fn only_the_complete_unmodified_single_video_is_eligible() {
    let base = plain_project();
    assert!(select(&base).is_ok());
    let mut p = base.clone();
    p.clips[0].speed = 2.;
    assert!(select(&p).is_err());
    let mut p = base.clone();
    p.clips[0].transform.x = 0.1;
    assert!(select(&p).is_err());
    let mut p = base.clone();
    p.clips[0].fade_out = 3;
    assert!(select(&p).is_err());
    let mut p = base.clone();
    p.width = 1080;
    assert!(select(&p).is_err());
    let mut p = base.clone();
    p.tracks[0].muted = true;
    assert!(select(&p).is_err());
    let mut p = base.clone();
    p.clips[0].start = 1;
    assert!(select(&p).is_err());
    let mut p = base.clone();
    let mut title = p.clips[0].clone();
    title.id = 4;
    title.content = Content::Title {
        text: "Overlay".into(),
        color: "white".into(),
        font_size: 32,
    };
    p.clips.push(title);
    assert!(select(&p).is_err());
    let mut p = base;
    p.clips[0].content = Content::Title {
        text: "Only title".into(),
        color: "white".into(),
        font_size: 32,
    };
    assert!(select(&p).is_err());
}
#[test]
fn changed_or_missing_source_is_a_typed_terminal_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("source.mkv");
    std::fs::write(&path, b"owned").unwrap();
    let stamp = SourceStamp::read(&path).unwrap();
    assert!(unchanged(&path, &stamp).is_ok());
    std::fs::write(&path, b"different owned bytes").unwrap();
    assert!(unchanged(&path, &stamp).unwrap_err().is::<SourceChanged>());
    std::fs::remove_file(&path).unwrap();
    assert!(unchanged(&path, &stamp).unwrap_err().is::<SourceChanged>());
}
