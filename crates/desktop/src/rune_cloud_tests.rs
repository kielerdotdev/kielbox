use super::*;
use crate::rune_ui;
use rune::{Env, Key, KeyEvent, Runtime};
use std::time::UNIX_EPOCH;

fn prefs(url: &str, auto: bool) -> Preferences {
    Preferences {
        cloud_url: url.into(),
        cloud_auto_upload: auto,
        ..Preferences::default()
    }
}

fn at(seconds: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(seconds)
}

fn known(modified_seconds: u64) -> Known {
    Known {
        item_id: "01ITEM".into(),
        size: 10,
        modified_ms: cloud::unix_ms(at(modified_seconds)),
    }
}

fn apply(state: &mut State, event: Event) -> Vec<Outcome> {
    let mut out = Vec::new();
    state.apply(event, &mut out);
    out
}

#[test]
fn a_card_shows_activity_first_then_whether_the_file_is_still_the_uploaded_one() {
    let mut state = State::new(&prefs("https://c.example.com", false), false);
    let clip = PathBuf::from("clip.mp4");
    assert_eq!(state.card(&clip, Some(at(100))), Card::NotUploaded);

    state.known.insert(clip.clone(), known(100));
    assert_eq!(state.card(&clip, Some(at(100))), Card::Uploaded);
    assert_eq!(state.card(&clip, None), Card::Uploaded, "not scanned yet");
    assert_eq!(
        state.card(&clip, Some(at(101))),
        Card::NotUploaded,
        "edited since the upload"
    );

    state.activity.insert(clip.clone(), Activity::Queued);
    assert_eq!(state.card(&clip, Some(at(100))), Card::Queued);
    state
        .activity
        .insert(clip.clone(), Activity::Uploading { sent: 0, total: 0 });
    assert_eq!(state.card(&clip, None), Card::Uploading(0));
    state.activity.insert(
        clip.clone(),
        Activity::Uploading {
            sent: 421,
            total: 1000,
        },
    );
    assert_eq!(state.card(&clip, None), Card::Uploading(42));
    state.activity.insert(
        clip.clone(),
        Activity::Uploading {
            sent: 2000,
            total: 1000,
        },
    );
    assert_eq!(state.card(&clip, None), Card::Uploading(100));
    state.activity.insert(clip.clone(), Activity::Sharing);
    assert_eq!(state.card(&clip, None), Card::Sharing);
    state
        .activity
        .insert(clip.clone(), Activity::Failed("boom".into()));
    assert_eq!(state.card(&clip, None), Card::Failed("boom".into()));
}

#[test]
fn worker_events_drive_the_card_and_the_notices() {
    let mut state = State::new(&prefs("https://c.example.com", false), false);
    let clip = PathBuf::from("clip.mp4");
    state.activity.insert(clip.clone(), Activity::Queued);

    apply(&mut state, Event::Started { path: clip.clone() });
    apply(
        &mut state,
        Event::Progress {
            path: clip.clone(),
            sent: 50,
            total: Some(100),
        },
    );
    assert_eq!(state.card(&clip, None), Card::Uploading(50));
    // A late progress report after the upload ended must not resurrect the spinner.
    apply(
        &mut state,
        Event::Uploaded {
            path: clip.clone(),
            item_id: "01ITEM".into(),
            size: 100,
            modified_ms: cloud::unix_ms(at(7)),
        },
    );
    apply(
        &mut state,
        Event::Progress {
            path: clip.clone(),
            sent: 100,
            total: Some(100),
        },
    );
    assert_eq!(state.card(&clip, Some(at(7))), Card::Uploaded);

    // Copy share link: the worker answers with the link, the UI copies it.
    assert!(state.activity.is_empty());
    state.activity.insert(clip.clone(), Activity::Sharing);
    let out = apply(
        &mut state,
        Event::Shared {
            path: clip.clone(),
            url: "https://share.example.com/s/T".into(),
        },
    );
    assert_eq!(
        out,
        vec![Outcome::Copy("https://share.example.com/s/T".into())]
    );
    assert_eq!(state.card(&clip, Some(at(7))), Card::Uploaded);

    // A failed link leaves the upload alone; a failed upload offers a retry.
    state.activity.insert(clip.clone(), Activity::Sharing);
    let out = apply(
        &mut state,
        Event::Failed {
            path: clip.clone(),
            message: "HTTP 500".into(),
        },
    );
    assert!(matches!(&out[..], [Outcome::Notice(NoticeKind::Error, m)] if m.contains("HTTP 500")));
    assert_eq!(state.card(&clip, Some(at(7))), Card::Uploaded);
    let other = PathBuf::from("other.mp4");
    state.activity.insert(other.clone(), Activity::Queued);
    apply(
        &mut state,
        Event::Failed {
            path: other.clone(),
            message: "network".into(),
        },
    );
    assert!(matches!(state.card(&other, None), Card::Failed(m) if m.contains("network")));

    // A cancelled upload returns to "not uploaded"; a changed file forgets its upload.
    state.activity.insert(other.clone(), Activity::Queued);
    apply(
        &mut state,
        Event::Cancelled {
            path: other.clone(),
        },
    );
    assert_eq!(state.card(&other, None), Card::NotUploaded);
    let out = apply(&mut state, Event::Stale { path: clip.clone() });
    assert!(matches!(&out[..], [Outcome::Notice(NoticeKind::Error, _)]));
    assert_eq!(state.card(&clip, None), Card::NotUploaded);
}

#[test]
fn a_successful_test_clears_the_typed_secret_and_remembers_where_it_went() {
    let mut state = State::new(&prefs("", false), false);
    state.client_id = TextEdit::new("id");
    state.client_secret = TextEdit::new("hunter2").secure();
    apply(
        &mut state,
        Event::Tested(Ok(cloud::Tested {
            identity: "kielbox-desktop".into(),
            saved_in: Some("Windows Credential Manager".into()),
        })),
    );
    assert_eq!(state.test, Test::Connected("kielbox-desktop".into()));
    assert!(state.client_secret.text.is_empty() && state.client_id.text.is_empty());
    apply(
        &mut state,
        Event::Credentials {
            saved_in: Some("Windows Credential Manager".into()),
        },
    );
    assert_eq!(
        state.saved_in.as_deref(),
        Some("Windows Credential Manager")
    );
    assert!(state.client_secret.is_secure());

    let out = apply(
        &mut state,
        Event::Tested(Err("Access rejected the token".into())),
    );
    assert!(out.is_empty());
    assert_eq!(state.test, Test::Failed("Access rejected the token".into()));
}

#[test]
fn only_captures_that_appear_after_the_first_scan_are_uploaded_automatically() {
    let now = at(1_000_000);
    let old = PathBuf::from("old.mp4");
    let fresh = PathBuf::from("new.png");
    let stale = PathBuf::from("restored.mp4");
    let imported = PathBuf::from("imported.mp4");
    let mut times = BTreeMap::new();
    times.insert(old.clone(), at(1_000_000 - 86_400));
    times.insert(fresh.clone(), at(1_000_000 - 20));
    times.insert(stale.clone(), at(1_000_000 - 3_600));
    times.insert(imported.clone(), at(1_000_000 - 5));

    let mut p = prefs("https://c.example.com", false);
    let mut state = State::new(&p, false);
    // Startup scan: records what is there and uploads nothing, even with the switch on.
    p.cloud_auto_upload = true;
    assert!(
        state
            .new_captures(&p, std::slice::from_ref(&old), &times, now)
            .is_empty()
    );

    // Later scan with the switch on.
    p.imported_files.insert(imported.clone());
    let files = [old.clone(), fresh.clone(), stale.clone(), imported.clone()];
    assert_eq!(
        state.new_captures(&p, &files, &times, now),
        vec![fresh.clone()]
    );
    // The same files on the next scan are no longer new.
    assert!(state.new_captures(&p, &files, &times, now).is_empty());

    // With the switch off a new capture is only recorded, so turning it on later does not
    // upload the library.
    p.cloud_auto_upload = false;
    let newer = PathBuf::from("newer.mp4");
    times.insert(newer.clone(), at(1_000_000 - 1));
    let files = [old, fresh, stale, imported, newer.clone()];
    assert!(state.new_captures(&p, &files, &times, now).is_empty());
    p.cloud_auto_upload = true;
    assert!(state.new_captures(&p, &files, &times, now).is_empty());

    // Not configured, or already uploaded or busy: skipped.
    let mut state = State::new(&p, false);
    state.new_captures(&p, &[], &times, now);
    state.known.insert(newer.clone(), known(999_999));
    assert!(
        state
            .new_captures(&p, std::slice::from_ref(&newer), &times, now)
            .is_empty()
    );
    let mut off = State::new(&p, false);
    off.new_captures(&p, &[], &times, now);
    p.cloud_url.clear();
    assert!(off.new_captures(&p, &[newer], &times, now).is_empty());
}

#[test]
fn a_disabled_state_never_starts_a_worker() {
    let p = prefs("https://c.example.com", true);
    let mut state = State::new(&p, false);
    state.start(std::sync::Arc::new(|| {}), &p);
    state.upload(&p, PathBuf::from("clip.mp4"));
    assert!(state.activity.is_empty(), "nothing queued without a worker");
    state.test(&p, "https://c.example.com");
    assert!(matches!(state.test, Test::Failed(_)));
    state.shutdown();
}

// ---- rendered UI ------------------------------------------------------------------------

fn runtime(configure: impl FnOnce(&mut rune_ui::State)) -> Runtime<rune_ui::State> {
    let mut state = rune_ui::State::with_headless(true);
    configure(&mut state);
    let mut rt = Runtime::new(state, rune_ui::view, (1200.0, 800.0), Env::new(true, 1.0));
    rt.frame(0.0);
    rt
}

fn click(rt: &mut Runtime<rune_ui::State>, key: &str) {
    let (x, y) = rt
        .find(key)
        .unwrap_or_else(|| panic!("Missing clickable element: {key}"));
    rt.pointer_down(x, y);
    rt.pointer_up(x, y);
    for _ in 0..40 {
        rt.frame(1.0 / 60.0);
    }
}

fn with_library(s: &mut rune_ui::State, url: &str) {
    s.preferences.cloud_url = url.into();
    s.files = ["a.png", "b.mp4", "c.mp4", "d.mp4", "e.mp4", "f.png"]
        .map(PathBuf::from)
        .to_vec();
}

#[test]
fn cards_show_a_cloud_button_only_when_a_cloud_is_configured() {
    let rt = runtime(|s| with_library(s, ""));
    assert!(rt.find("capture-0").is_some());
    assert!(rt.find("cloud-0").is_none());
    drop(rt);

    let mut rt = runtime(|s| {
        with_library(s, "https://cloud.example.com");
        let p = &s.files;
        s.cloud.activity.insert(p[1].clone(), Activity::Queued);
        s.cloud.activity.insert(
            p[2].clone(),
            Activity::Uploading {
                sent: 42,
                total: 100,
            },
        );
        s.cloud.known.insert(p[3].clone(), known(1));
        s.cloud
            .activity
            .insert(p[4].clone(), Activity::Failed("nope".into()));
        s.cloud.activity.insert(p[5].clone(), Activity::Sharing);
    });
    for i in 0..6 {
        assert!(
            rt.find(&format!("cloud-{i}")).is_some(),
            "card {i} has its cloud button"
        );
    }
    // Pressing the idle button with no worker leaves the card as it was.
    click(&mut rt, "cloud-0");
    assert_eq!(
        rt.state.cloud.card(&PathBuf::from("a.png"), None),
        Card::NotUploaded
    );
}

#[test]
fn the_cloud_settings_tab_is_reachable_and_edits_preferences() {
    let mut rt = runtime(|_| {});
    click(&mut rt, "nav-settings");
    click(&mut rt, "settings-tab-5");
    assert_eq!(rt.state.settings_tab, 5);
    for key in [
        "cloud-url",
        "cloud-client-id",
        "cloud-client-secret",
        "cloud-test",
        "cloud-auto-upload",
        "cloud-share-days",
        "cloud-open-web",
    ] {
        assert!(rt.find(key).is_some(), "{key} is on the tab");
    }
    assert!(!rt.state.preferences.cloud_auto_upload);
    click(&mut rt, "cloud-auto-upload");
    assert!(rt.state.preferences.cloud_auto_upload);
    click(&mut rt, "cloud-share-days-2");
    assert_eq!(rt.state.preferences.cloud_share_days, 30);
    click(&mut rt, "cloud-share-days-3");
    assert_eq!(rt.state.preferences.cloud_share_days, 0);

    // Typing the address and testing: rejects a non-URL without touching preferences.
    click(&mut rt, "cloud-url");
    rt.key(KeyEvent {
        key: Key::Text("not a url".into()),
        mods: Default::default(),
    });
    rt.frame(0.0);
    click(&mut rt, "cloud-test");
    assert!(matches!(rt.state.cloud.test, Test::Failed(_)));
    assert!(rt.state.preferences.cloud_url.is_empty());
}

#[test]
fn the_secret_field_is_masked_and_a_saved_token_hides_the_inputs() {
    let mut rt = runtime(|_| {});
    click(&mut rt, "nav-settings");
    click(&mut rt, "settings-tab-5");
    click(&mut rt, "cloud-client-secret");
    rt.key(KeyEvent {
        key: Key::Text("s3cret-value".into()),
        mods: Default::default(),
    });
    rt.frame(0.0);
    assert_eq!(rt.state.cloud.client_secret.text, "s3cret-value");
    assert!(rt.state.cloud.client_secret.is_secure());
    assert!(!format!("{:?}", rt.state.cloud.client_secret).contains("s3cret-value"));

    let mut out = Vec::new();
    rt.state.cloud.apply(
        Event::Credentials {
            saved_in: Some("Windows Credential Manager".into()),
        },
        &mut out,
    );
    rt.invalidate();
    for _ in 0..5 {
        rt.frame(1.0 / 60.0);
    }
    assert!(rt.find("cloud-client-secret").is_none());
    assert!(rt.find("cloud-replace").is_some());
    click(&mut rt, "cloud-replace");
    assert!(rt.find("cloud-client-secret").is_some());
}

#[test]
fn the_wine_fixture_scenes_cover_the_tab_and_every_card_state() {
    let rt = runtime(crate::rune_fixtures::cloud_settings);
    assert_eq!(rt.state.settings_tab, 5);
    assert!(rt.find("cloud-replace").is_some() && rt.find("cloud-test").is_some());

    let rt = runtime(crate::rune_fixtures::cloud_cards);
    let s = &rt.state;
    let states: Vec<Card> = s
        .files
        .iter()
        .map(|p| s.cloud.card(p, s.file_times.get(p).copied()))
        .collect();
    assert_eq!(
        states,
        vec![
            Card::NotUploaded,
            Card::Queued,
            Card::Uploading(42),
            Card::Uploaded,
            Card::Failed("Cloud: network error".into()),
            Card::Sharing
        ]
    );
    for i in 0..states.len() {
        assert!(rt.find(&format!("cloud-{i}")).is_some(), "card {i}");
    }
}
