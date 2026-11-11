use super::*;
use serde_json::{Value, json};
use std::time::Instant;
use upload::InMemorySecretStore;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const ID: &str = "client-id-123";
const SECRET: &str = "super-secret-value";
const FAR_FUTURE_MS: i64 = 4_102_444_800_000;

// ---- state file -------------------------------------------------------------------------

#[test]
fn state_file_round_trips_through_an_atomic_save() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("projects").join("cloud.json");
    let mut state = StateFile::default();
    state.items.insert(
        "C:\\captures\\a.mp4".into(),
        Entry {
            item_id: "01ITEM".into(),
            size: 42,
            modified_ms: 1_700_000_000_123,
            share: Some(ShareRecord {
                id: "S1".into(),
                url: "https://share.example.com/s/TOKEN".into(),
                expires_at_ms: Some(FAR_FUTURE_MS),
            }),
        },
    );
    state.items.insert(
        "b.png".into(),
        Entry {
            item_id: "01OTHER".into(),
            size: 7,
            modified_ms: 5,
            share: None,
        },
    );
    state.save(&file).unwrap();
    let (loaded, warning) = StateFile::load(&file);
    assert_eq!(warning, None);
    assert_eq!(loaded, state);
    // Overwriting leaves exactly one file behind: no temporary file survives the rename.
    state.items.remove("b.png");
    state.save(&file).unwrap();
    assert_eq!(StateFile::load(&file).0, state);
    let names: Vec<_> = std::fs::read_dir(file.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names, vec![std::ffi::OsString::from("cloud.json")]);
    // The documented shape.
    let json: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    assert_eq!(json["version"], 1);
    assert_eq!(json["items"]["C:\\captures\\a.mp4"]["item_id"], "01ITEM");
    assert_eq!(json["items"]["C:\\captures\\a.mp4"]["share"]["id"], "S1");
}

#[test]
fn a_missing_state_file_is_empty_and_an_unreadable_one_is_set_aside() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("cloud.json");
    let (state, warning) = StateFile::load(&file);
    assert!(state.items.is_empty() && warning.is_none());

    std::fs::write(&file, b"{ not json").unwrap();
    let (state, warning) = StateFile::load(&file);
    assert!(state.items.is_empty());
    assert!(warning.unwrap().contains("set aside"));
    assert!(!file.exists());
    assert_eq!(
        std::fs::read(dir.path().join("cloud.json.unreadable")).unwrap(),
        b"{ not json"
    );

    // A file from a newer Kielbox is kept, not overwritten by the next save.
    std::fs::write(&file, br#"{"version":2,"items":{}}"#).unwrap();
    let (_, warning) = StateFile::load(&file);
    assert!(warning.is_some());
    assert!(dir.path().join("cloud.json.unreadable").exists());
}

#[test]
fn an_entry_is_uploaded_only_while_size_and_mtime_still_match() {
    let dir = tempfile::tempdir().unwrap();
    let clip = dir.path().join("clip.mp4");
    std::fs::write(&clip, b"0123456789").unwrap();
    let uploaded = Stamp::of(&clip).unwrap();
    assert_eq!(uploaded.size, 10);

    let mut state = StateFile::default();
    state.items.insert(
        StateFile::key(&clip),
        Entry {
            item_id: "01ITEM".into(),
            size: uploaded.size,
            modified_ms: uploaded.modified_ms,
            share: None,
        },
    );
    assert!(state.current(&clip, uploaded).is_some());
    assert_eq!(state.snapshot().len(), 1);

    // Same size, different mtime (a rewrite in place, an annotation save).
    let file = std::fs::OpenOptions::new().write(true).open(&clip).unwrap();
    file.set_modified(SystemTime::now() + Duration::from_secs(120))
        .unwrap();
    let touched = Stamp::of(&clip).unwrap();
    assert_eq!(touched.size, uploaded.size);
    assert_ne!(touched.modified_ms, uploaded.modified_ms);
    assert!(state.current(&clip, touched).is_none());
    assert!(state.snapshot().is_empty());

    // Different size, original mtime restored.
    std::fs::write(&clip, b"0123456789 and more").unwrap();
    let file = std::fs::OpenOptions::new().write(true).open(&clip).unwrap();
    file.set_modified(UNIX_EPOCH + Duration::from_millis(uploaded.modified_ms as u64))
        .unwrap();
    let grown = Stamp::of(&clip).unwrap();
    assert_eq!(grown.modified_ms, uploaded.modified_ms);
    assert!(state.current(&clip, grown).is_none());

    // A missing file is not reported, but its record stays in the state.
    std::fs::remove_file(&clip).unwrap();
    assert!(state.snapshot().is_empty());
    assert_eq!(state.items.len(), 1);
}

#[test]
fn share_links_expire_with_a_margin_and_stay_out_of_debug_output() {
    let share = ShareRecord {
        id: "S".into(),
        url: "https://share.example.com/s/TOKEN".into(),
        expires_at_ms: Some(1_000_000),
    };
    assert!(share.usable(1_000_000 - 2 * SHARE_EXPIRY_MARGIN_MS));
    assert!(!share.usable(1_000_000 - SHARE_EXPIRY_MARGIN_MS / 2));
    assert!(!share.usable(2_000_000));
    assert!(
        ShareRecord {
            expires_at_ms: None,
            ..share.clone()
        }
        .usable(i64::MAX / 2)
    );
    assert!(!format!("{share:?}").contains("TOKEN"));
}

#[test]
fn credentials_debug_hides_the_secret() {
    let creds = Credentials {
        client_id: ID.into(),
        client_secret: SECRET.into(),
    };
    let shown = format!("{creds:?}");
    assert!(shown.contains(ID) && !shown.contains(SECRET));
}

// ---- queue ------------------------------------------------------------------------------

#[test]
fn queue_runs_in_order_one_at_a_time_and_ignores_duplicates() {
    let mut queue = Queue::default();
    let (a, b, c) = (PathBuf::from("a"), PathBuf::from("b"), PathBuf::from("c"));
    assert!(queue.enqueue(a.clone()));
    assert!(queue.enqueue(b.clone()));
    assert!(queue.enqueue(c.clone()));
    assert!(!queue.enqueue(b.clone()), "already waiting");
    assert_eq!(queue.next(), Some(a.clone()));
    assert!(!queue.enqueue(a.clone()), "already running");
    assert_eq!(queue.next(), None, "one at a time");
    assert_eq!(queue.len(), 3);
    queue.finish();
    assert_eq!(queue.next(), Some(b));
    queue.finish();
    assert_eq!(queue.next(), Some(c));
    queue.finish();
    assert_eq!(queue.next(), None);
    assert!(queue.is_empty());
    assert!(queue.enqueue(a), "a finished path can be queued again");
}

#[test]
fn queue_cancel_removes_waiting_work_and_flags_the_running_upload() {
    let mut queue = Queue::default();
    let (a, b, c) = (PathBuf::from("a"), PathBuf::from("b"), PathBuf::from("c"));
    for p in [&a, &b, &c] {
        queue.enqueue(p.clone());
    }
    queue.next();
    assert_eq!(queue.cancel(&b), Removed::Waiting);
    assert_eq!(queue.cancel(&b), Removed::Unknown);
    assert_eq!(queue.cancel(&a), Removed::Active);
    assert_eq!(
        queue.active(),
        Some(a.as_path()),
        "stays active until it ends"
    );
    queue.finish();
    assert_eq!(queue.next(), Some(c));
}

#[test]
fn kinds_follow_the_library_filters() {
    assert_eq!(kind_for(Path::new("shot.png")), ItemKind::Screenshot);
    assert_eq!(kind_for(Path::new("Replay 2026.mkv")), ItemKind::Replay);
    assert_eq!(kind_for(Path::new("recording-1.mp4")), ItemKind::Recording);
    assert_eq!(kind_for(Path::new("notes.txt")), ItemKind::Other);
}

#[test]
fn thumbnails_are_valid_jpegs() {
    let rgba: Vec<u8> = (0..16 * 9).flat_map(|i| [i as u8, 90, 200, 255]).collect();
    let jpeg = encode_jpeg(16, 9, &rgba).unwrap();
    assert_eq!(&jpeg[..3], &[0xff, 0xd8, 0xff]);
    assert!(encode_jpeg(16, 9, &rgba[..10]).is_none());
}

// ---- worker -----------------------------------------------------------------------------

struct Rig {
    handle: Handle,
    state_path: PathBuf,
    store: Arc<InMemorySecretStore>,
    seen: Vec<Event>,
    dir: tempfile::TempDir,
}

fn rig(api: &MockServer, with_token: bool, grace: Duration) -> Rig {
    rig_at(api, with_token, grace, None)
}

/// A worker reading `state_path` (or a fresh `cloud.json` in the rig's own folder).
fn rig_at(api: &MockServer, with_token: bool, grace: Duration, state_path: Option<&Path>) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let state_path = state_path.map_or_else(|| dir.path().join("cloud.json"), Path::to_path_buf);
    let store = Arc::new(InMemorySecretStore::new());
    if with_token {
        store.set(CLIENT_ID_KEY, ID).unwrap();
        store.set(CLIENT_SECRET_KEY, SECRET).unwrap();
    }
    let secrets = store.clone();
    let handle = Handle::spawn(WorkerConfig {
        state_path: state_path.clone(),
        secrets: Box::new(move || Secrets {
            store: secrets,
            location: "test store".into(),
        }),
        make_client: Arc::new(|config| {
            Ok(CloudClient::new(config)?
                .without_proxy()?
                .with_retry_backoff(Duration::from_millis(5)))
        }),
        wake: Arc::new(|| {}),
        grace,
    })
    .unwrap();
    assert!(handle.send(Command::Configure {
        url: api.uri(),
        share_days: 7,
    }));
    Rig {
        handle,
        state_path,
        store,
        seen: Vec::new(),
        dir,
    }
}

impl Rig {
    fn file(&self, name: &str, len: usize) -> PathBuf {
        let path = self.dir.path().join(name);
        std::fs::write(&path, vec![b'x'; len]).unwrap();
        path
    }

    /// Wait for the first not-yet-consumed event that satisfies `want`.
    async fn wait(&mut self, what: &str, want: impl Fn(&Event) -> bool) -> Event {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            self.seen.extend(self.handle.poll());
            if let Some(i) = self.seen.iter().position(&want) {
                return self.seen.remove(i);
            }
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    async fn uploaded(&mut self, path: &Path) -> Event {
        let path = path.to_path_buf();
        self.wait(
            "Uploaded",
            move |e| matches!(e, Event::Uploaded { path: p, .. } if *p == path),
        )
        .await
    }
}

fn item_json(id: &str) -> Value {
    json!({
        "id": id, "status": "ready", "kind": "recording", "title": "t",
        "filename": "f.mp4", "content_type": "video/mp4", "size": 1,
        "captured_at": 1, "created_at": 2, "created_by": "svc", "favorite": false,
        "media_url": format!("/api/items/{id}/media"), "thumb_url": null
    })
}

/// Mount the API side of a single-PUT upload of `name` as item `id`, PUT to
/// `{storage}/put/{name}` answering after `put_delay`.
async fn mount_upload(
    api: &MockServer,
    storage: &MockServer,
    name: &str,
    id: &str,
    put_delay: Duration,
) {
    Mock::given(method("POST"))
        .and(path("/api/uploads"))
        .and(wiremock::matchers::body_partial_json(
            json!({"filename": name}),
        ))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "item_id": id,
            "upload": {"mode":"single","url":format!("{}/put/{name}", storage.uri()),
                       "headers":{"content-type":"video/mp4"},"expires_at":1}
        })))
        .mount(api)
        .await;
    Mock::given(method("PUT"))
        .and(path(format!("/put/{name}")))
        .respond_with(ResponseTemplate::new(200).set_delay(put_delay))
        .mount(storage)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("/api/uploads/{id}/complete")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"item": item_json(id)})))
        .mount(api)
        .await;
    Mock::given(method("DELETE"))
        .and(path(format!("/api/uploads/{id}")))
        .respond_with(ResponseTemplate::new(204))
        .mount(api)
        .await;
}

async fn received(server: &MockServer, verb: &str, p: &str) -> Vec<wiremock::Request> {
    server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.method.as_str() == verb && r.url.path() == p)
        .collect()
}

async fn until<F: std::future::Future<Output = bool>>(what: &str, mut check: impl FnMut() -> F) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !check().await {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn upload_reports_progress_records_the_file_and_survives_a_restart() {
    let (api, storage) = (MockServer::start().await, MockServer::start().await);
    mount_upload(&api, &storage, "clip.mp4", "ITEM1", Duration::ZERO).await;
    let mut rig = rig(&api, true, SHUTDOWN_GRACE);
    let clip = rig.file("clip.mp4", 200_000);

    rig.wait(
        "Loaded",
        |e| matches!(e, Event::Loaded { known, .. } if known.is_empty()),
    )
    .await;
    rig.wait(
        "Credentials",
        |e| matches!(e, Event::Credentials { saved_in: Some(s) } if s == "test store"),
    )
    .await;
    assert!(rig.handle.send(Command::Upload(clip.clone())));
    rig.wait("Started", |e| matches!(e, Event::Started { .. }))
        .await;
    let Event::Uploaded {
        item_id,
        size,
        modified_ms,
        ..
    } = rig.uploaded(&clip).await
    else {
        unreachable!()
    };
    assert_eq!((item_id.as_str(), size), ("ITEM1", 200_000));
    assert!(
        rig.seen
            .iter()
            .any(|e| matches!(e, Event::Progress { sent, .. } if *sent > 0)),
        "progress is reported"
    );

    // The token went to the API origin only, and the file was streamed intact.
    let create = &received(&api, "POST", "/api/uploads").await[0];
    assert_eq!(create.headers.get("cf-access-client-id").unwrap(), ID);
    let put = &received(&storage, "PUT", "/put/clip.mp4").await[0];
    assert_eq!(put.body.len(), 200_000);
    assert!(!put.headers.contains_key("cf-access-client-id"));

    let (state, _) = StateFile::load(&rig.state_path);
    let entry = &state.items[&StateFile::key(&clip)];
    assert_eq!(
        (entry.item_id.as_str(), entry.size, entry.modified_ms),
        ("ITEM1", 200_000, modified_ms)
    );
    assert!(
        !std::fs::read_to_string(&rig.state_path)
            .unwrap()
            .contains(SECRET)
    );

    // A fresh worker reads the file and reports the upload; changing the file hides it.
    let mut again = rig_at(&api, true, SHUTDOWN_GRACE, Some(&rig.state_path));
    again
        .wait(
            "Loaded",
            |e| matches!(e, Event::Loaded { known, .. } if known.len() == 1),
        )
        .await;
    drop(again);
    std::fs::write(&clip, b"edited in place").unwrap();
    let mut third = rig_at(&api, true, SHUTDOWN_GRACE, Some(&rig.state_path));
    third
        .wait(
            "Loaded",
            |e| matches!(e, Event::Loaded { known, .. } if known.is_empty()),
        )
        .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn uploads_run_one_at_a_time_in_the_order_they_were_requested() {
    let (api, storage) = (MockServer::start().await, MockServer::start().await);
    let names = ["a.mp4", "b.mp4", "c.mp4", "d.mp4"];
    for (i, name) in names.iter().enumerate() {
        mount_upload(
            &api,
            &storage,
            name,
            &format!("ITEM{i}"),
            Duration::from_millis(150),
        )
        .await;
    }
    let mut rig = rig(&api, true, SHUTDOWN_GRACE);
    let files: Vec<_> = names.iter().map(|n| rig.file(n, 1000)).collect();
    for file in &files {
        rig.handle.send(Command::Upload(file.clone()));
    }
    let mut finished = Vec::new();
    for _ in 0..files.len() {
        let event = rig
            .wait("an upload", |e| matches!(e, Event::Uploaded { .. }))
            .await;
        // Never two in flight: when one finishes, nothing later has started its PUT yet.
        // Everything was requested at once; only the finished ones and at most the next
        // one have been started.
        let started = received(&api, "POST", "/api/uploads").await.len();
        assert!(started <= finished.len() + 2, "{started} started");
        if let Event::Uploaded { path, .. } = event {
            finished.push(path);
        }
    }
    assert_eq!(finished, files, "first in, first out");
    let order: Vec<String> = received(&api, "POST", "/api/uploads")
        .await
        .iter()
        .map(|r| {
            serde_json::from_slice::<Value>(&r.body).unwrap()["filename"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect();
    assert_eq!(order, names);
}

#[tokio::test(flavor = "multi_thread")]
async fn cancel_stops_the_running_upload_and_drops_queued_ones() {
    let (api, storage) = (MockServer::start().await, MockServer::start().await);
    mount_upload(&api, &storage, "slow.mp4", "SLOW", Duration::from_secs(60)).await;
    mount_upload(&api, &storage, "waiting.mp4", "WAIT", Duration::ZERO).await;
    mount_upload(&api, &storage, "last.mp4", "LAST", Duration::ZERO).await;
    let mut rig = rig(&api, true, SHUTDOWN_GRACE);
    let (slow, waiting, last) = (
        rig.file("slow.mp4", 5000),
        rig.file("waiting.mp4", 5000),
        rig.file("last.mp4", 5000),
    );
    for f in [&slow, &waiting, &last] {
        rig.handle.send(Command::Upload(f.clone()));
    }
    until("the slow PUT to start", || async {
        !received(&storage, "PUT", "/put/slow.mp4").await.is_empty()
    })
    .await;

    // Cancel a queued one first: it leaves without ever contacting the server.
    rig.handle.send(Command::Cancel(waiting.clone()));
    let queued = waiting.clone();
    rig.wait(
        "queued upload cancelled",
        move |e| matches!(e, Event::Cancelled { path } if *path == queued),
    )
    .await;

    // Then the running one: its half-finished item is discarded and the queue moves on.
    rig.handle.send(Command::Cancel(slow.clone()));
    let running = slow.clone();
    rig.wait(
        "running upload cancelled",
        move |e| matches!(e, Event::Cancelled { path } if *path == running),
    )
    .await;
    rig.uploaded(&last).await;
    assert_eq!(received(&api, "DELETE", "/api/uploads/SLOW").await.len(), 1);
    assert!(
        received(&api, "POST", "/api/uploads")
            .await
            .iter()
            .all(|r| {
                serde_json::from_slice::<Value>(&r.body).unwrap()["filename"] != "waiting.mp4"
            })
    );
    assert!(!rig.seen.iter().any(|e| matches!(e, Event::Failed { .. })));
    let (state, _) = StateFile::load(&rig.state_path);
    assert_eq!(state.items.len(), 1);
    assert!(state.items.contains_key(&StateFile::key(&last)));
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_with_an_upload_in_flight_aborts_it_and_returns_promptly() {
    let (api, storage) = (MockServer::start().await, MockServer::start().await);
    // The storage server stalls: the PUT would answer after a minute.
    mount_upload(&api, &storage, "big.mp4", "BIG", Duration::from_secs(60)).await;
    let mut rig = rig(&api, true, SHUTDOWN_GRACE);
    let big = rig.file("big.mp4", 50_000);
    rig.handle.send(Command::Upload(big.clone()));
    until("the stalled PUT to arrive", || async {
        !received(&storage, "PUT", "/put/big.mp4").await.is_empty()
    })
    .await;

    let started = Instant::now();
    let stopped = rig.handle.shutdown();
    let took = started.elapsed();
    assert!(stopped, "the worker thread ended and was joined");
    assert!(took < Duration::from_secs(5), "shutdown took {took:?}");
    assert_eq!(
        received(&api, "DELETE", "/api/uploads/BIG").await.len(),
        1,
        "abort call sent"
    );
    assert!(
        received(&api, "POST", "/api/uploads/BIG/complete")
            .await
            .is_empty()
    );
    rig.seen.extend(rig.handle.poll());
    assert!(!rig.seen.iter().any(|e| matches!(e, Event::Uploaded { .. })));
    let (state, _) = StateFile::load(&rig.state_path);
    assert!(state.items.is_empty(), "an aborted upload is not recorded");
    // Shutting down again is harmless.
    assert!(rig.handle.shutdown());
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_gives_up_after_the_grace_period_when_the_abort_call_stalls_too() {
    let (api, storage) = (MockServer::start().await, MockServer::start().await);
    mount_upload(&api, &storage, "big.mp4", "BIG", Duration::from_secs(60)).await;
    Mock::given(method("DELETE"))
        .and(path("/api/uploads/BIG"))
        .respond_with(ResponseTemplate::new(204).set_delay(Duration::from_secs(60)))
        .with_priority(1)
        .mount(&api)
        .await;
    let mut rig = rig(&api, true, Duration::from_millis(400));
    let big = rig.file("big.mp4", 50_000);
    rig.handle.send(Command::Upload(big));
    until("the stalled PUT to arrive", || async {
        !received(&storage, "PUT", "/put/big.mp4").await.is_empty()
    })
    .await;
    let started = Instant::now();
    assert!(rig.handle.shutdown());
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "{:?}",
        started.elapsed()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn sharing_creates_a_link_once_then_reuses_it_and_notices_a_changed_file() {
    let (api, storage) = (MockServer::start().await, MockServer::start().await);
    mount_upload(&api, &storage, "clip.mp4", "ITEM1", Duration::ZERO).await;
    Mock::given(method("POST"))
        .and(path("/api/items/ITEM1/shares"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "share":{"id":"S1","item_id":"ITEM1","created_at":1,"expires_at":FAR_FUTURE_MS,
                     "revoked_at":null,"allow_download":true,"view_count":0},
            "url":"https://share.example.com/s/TOKEN"})))
        .mount(&api)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/shares/S1"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&api)
        .await;
    let mut rig = rig(&api, true, SHUTDOWN_GRACE);
    let clip = rig.file("clip.mp4", 3000);

    // Not uploaded yet: nothing to share.
    rig.handle.send(Command::Share(clip.clone()));
    rig.wait("stale", |e| matches!(e, Event::Stale { .. }))
        .await;

    rig.handle.send(Command::Upload(clip.clone()));
    rig.uploaded(&clip).await;
    for _ in 0..2 {
        rig.handle.send(Command::Share(clip.clone()));
        let event = rig
            .wait("Shared", |e| matches!(e, Event::Shared { .. }))
            .await;
        let Event::Shared { url, .. } = event else {
            unreachable!()
        };
        assert_eq!(url, "https://share.example.com/s/TOKEN");
    }
    let creates = received(&api, "POST", "/api/items/ITEM1/shares").await;
    assert_eq!(creates.len(), 1, "the second copy reuses the stored link");
    assert_eq!(
        serde_json::from_slice::<Value>(&creates[0].body).unwrap(),
        json!({"expires_in_seconds": 604_800})
    );
    let (state, _) = StateFile::load(&rig.state_path);
    assert_eq!(
        state.items[&StateFile::key(&clip)]
            .share
            .as_ref()
            .unwrap()
            .id,
        "S1"
    );

    rig.handle.send(Command::Revoke(clip.clone()));
    rig.wait("Revoked", |e| matches!(e, Event::Revoked { .. }))
        .await;
    assert_eq!(received(&api, "DELETE", "/api/shares/S1").await.len(), 1);
    assert!(
        StateFile::load(&rig.state_path).0.items[&StateFile::key(&clip)]
            .share
            .is_none()
    );

    std::fs::write(&clip, b"changed").unwrap();
    rig.handle.send(Command::Share(clip));
    rig.wait("stale after edit", |e| matches!(e, Event::Stale { .. }))
        .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn testing_saves_typed_credentials_only_when_they_work() {
    let api = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/me"))
        .and(header("cf-access-client-secret", SECRET))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "identity":"kielbox-desktop","kind":"service",
            "limits":{"max_upload_bytes":100,"multipart_threshold":64,"part_size":32}})))
        .with_priority(1)
        .mount(&api)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/me"))
        .respond_with(ResponseTemplate::new(401))
        .with_priority(5)
        .mount(&api)
        .await;
    let mut rig = rig(&api, false, SHUTDOWN_GRACE);
    rig.wait("empty credentials", |e| {
        matches!(e, Event::Credentials { saved_in: None })
    })
    .await;

    // Nothing typed and nothing saved.
    rig.handle.send(Command::Test {
        url: api.uri(),
        credentials: None,
    });
    let Event::Tested(result) = rig.wait("Tested", |e| matches!(e, Event::Tested(_))).await else {
        unreachable!()
    };
    assert!(result.unwrap_err().contains("service token"));

    // Wrong secret: rejected, and nothing is saved.
    rig.handle.send(Command::Test {
        url: api.uri(),
        credentials: Some(Credentials {
            client_id: ID.into(),
            client_secret: "wrong".into(),
        }),
    });
    let Event::Tested(result) = rig.wait("Tested", |e| matches!(e, Event::Tested(_))).await else {
        unreachable!()
    };
    let message = result.unwrap_err();
    assert!(!message.contains("wrong") && !message.contains(SECRET));
    assert_eq!(rig.store.get(CLIENT_SECRET_KEY).unwrap(), None);

    // Right secret: connected, saved under the documented keys.
    rig.handle.send(Command::Test {
        url: api.uri(),
        credentials: Some(Credentials {
            client_id: ID.into(),
            client_secret: SECRET.into(),
        }),
    });
    let Event::Tested(result) = rig.wait("Tested", |e| matches!(e, Event::Tested(_))).await else {
        unreachable!()
    };
    let tested = result.unwrap();
    assert_eq!(tested.identity, "kielbox-desktop");
    assert_eq!(tested.saved_in.as_deref(), Some("test store"));
    assert_eq!(rig.store.get(CLIENT_ID_KEY).unwrap().as_deref(), Some(ID));
    assert_eq!(
        rig.store.get(CLIENT_SECRET_KEY).unwrap().as_deref(),
        Some(SECRET)
    );
    rig.wait("saved credentials", |e| {
        matches!(e, Event::Credentials { saved_in: Some(_) })
    })
    .await;

    // Later tests use the saved token.
    rig.handle.send(Command::Test {
        url: api.uri(),
        credentials: None,
    });
    let Event::Tested(result) = rig.wait("Tested", |e| matches!(e, Event::Tested(_))).await else {
        unreachable!()
    };
    assert_eq!(result.unwrap().saved_in, None);
}

#[tokio::test(flavor = "multi_thread")]
async fn uploads_fail_clearly_without_an_address_or_a_token() {
    let api = MockServer::start().await;
    let mut rig = rig(&api, false, SHUTDOWN_GRACE);
    let clip = rig.file("clip.mp4", 10);
    rig.handle.send(Command::Upload(clip.clone()));
    let Event::Failed { message, .. } = rig
        .wait("Failed", |e| matches!(e, Event::Failed { .. }))
        .await
    else {
        unreachable!()
    };
    assert!(message.contains("service token"), "{message}");

    rig.handle.send(Command::Configure {
        url: String::new(),
        share_days: 7,
    });
    rig.store.set(CLIENT_ID_KEY, ID).unwrap();
    rig.store.set(CLIENT_SECRET_KEY, SECRET).unwrap();
    rig.handle.send(Command::Upload(clip));
    let Event::Failed { message, .. } = rig
        .wait("Failed", |e| matches!(e, Event::Failed { .. }))
        .await
    else {
        unreachable!()
    };
    assert!(message.contains("address"), "{message}");
    assert!(received(&api, "POST", "/api/uploads").await.is_empty());
}
