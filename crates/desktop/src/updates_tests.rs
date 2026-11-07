use super::*;
use std::sync::{
    Mutex,
    atomic::{AtomicBool, AtomicUsize},
};

fn asset() -> Asset {
    Asset {
        schema_version: 1,
        appid: "Kielbox".into(),
        channel: "preview".into(),
        version: "0.1.0-ci.2".into(),
        file: "Kielbox-0.1.0-ci.2-Setup.exe".into(),
        sha256: format!("{:x}", Sha256::digest(b"verified package")),
        size: b"verified package".len() as u64,
        notes: String::new(),
    }
}

fn update() -> Asset {
    asset()
}

struct FakeBackend {
    checks: AtomicUsize,
    downloads: AtomicUsize,
    applies: Mutex<Vec<(bool, PathBuf)>>,
    check_gate: Mutex<Option<Receiver<()>>>,
    fail_check: AtomicBool,
    panic_check: AtomicBool,
    fail_download: AtomicBool,
}

impl FakeBackend {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            checks: AtomicUsize::new(0),
            downloads: AtomicUsize::new(0),
            applies: Mutex::new(Vec::new()),
            check_gate: Mutex::new(None),
            fail_check: AtomicBool::new(false),
            panic_check: AtomicBool::new(false),
            fail_download: AtomicBool::new(false),
        })
    }
}

impl Backend for FakeBackend {
    fn check(&self, _: &AtomicBool) -> Result<CheckResult, String> {
        self.checks.fetch_add(1, Ordering::SeqCst);
        if let Some(gate) = self.check_gate.lock().unwrap().take() {
            gate.recv_timeout(Duration::from_secs(2)).unwrap();
        }
        assert!(
            !self.panic_check.swap(false, Ordering::SeqCst),
            "injected update worker failure"
        );
        if self.fail_check.load(Ordering::SeqCst) {
            return Err("offline fixture".into());
        }
        Ok(CheckResult::Available(update()))
    }
    fn download(
        &self,
        update: &Asset,
        progress: Arc<AtomicU8>,
        _: &AtomicBool,
    ) -> Result<Asset, String> {
        self.downloads.fetch_add(1, Ordering::SeqCst);
        progress.store(50, Ordering::Relaxed);
        if self.fail_download.load(Ordering::SeqCst) {
            return Err("interrupted download fixture".into());
        }
        Ok(update.clone())
    }
    fn apply(&self, _: &Asset, restart: bool, data_root: &Path) -> Result<(), String> {
        self.applies
            .lock()
            .unwrap()
            .push((restart, data_root.to_owned()));
        Ok(())
    }
}

fn controller(now: Instant, backend: Arc<FakeBackend>) -> Updates {
    let mut updates = Updates::unmanaged(now);
    updates.installed = true;
    updates.status = Status::Idle;
    updates.backend = Some(backend);
    updates
}

fn finish(updates: &mut Updates, now: Instant) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while updates.busy() {
        updates.poll(now, false);
        assert!(
            Instant::now() < deadline,
            "bounded fixture worker did not retire"
        );
        std::thread::yield_now();
    }
}

#[test]
fn update_feed_rejects_foreign_missing_digest_oversized_and_unsafe_assets() {
    validate_asset(&asset(), Channel::Preview).unwrap();
    let mut changed = asset();
    changed.appid = "AnotherApp".into();
    assert!(validate_asset(&changed, Channel::Preview).is_err());
    let mut changed = asset();
    changed.channel = "Delta".into();
    assert!(validate_asset(&changed, Channel::Preview).is_err());
    for name in [
        "../escape-full.nupkg",
        "C:\\escape-full.nupkg",
        "\\\\host\\escape-full.nupkg",
        "file:stream-full.nupkg",
        "not-a-package.exe",
    ] {
        let mut changed = asset();
        changed.file = name.into();
        assert!(
            validate_asset(&changed, Channel::Preview).is_err(),
            "accepted {name}"
        );
    }
    for digest in ["", "sha1-only", &"z".repeat(64)] {
        let mut changed = asset();
        changed.sha256 = digest.into();
        assert!(validate_asset(&changed, Channel::Preview).is_err());
    }
    for bytes in [0, MAX_PACKAGE_BYTES + 1] {
        let mut changed = asset();
        changed.size = bytes;
        assert!(validate_asset(&changed, Channel::Preview).is_err());
    }
}

#[test]
fn ready_package_is_rehashed_and_same_length_tampering_is_rejected() {
    let root = tempfile::tempdir().unwrap();
    let mut expected = asset();
    expected.sha256.make_ascii_uppercase();
    let path = root.path().join(&expected.file);
    std::fs::write(&path, b"verified package").unwrap();
    verify_package(root.path(), &expected).unwrap();
    std::fs::write(&path, b"modified package").unwrap();
    assert_eq!(std::fs::metadata(&path).unwrap().len(), expected.size);
    assert!(verify_package(root.path(), &expected).is_err());
    std::fs::write(&path, b"truncated").unwrap();
    assert!(verify_package(root.path(), &expected).is_err());
}

#[test]
fn damaged_expected_cache_entry_can_retry_without_touching_neighbor_files() {
    let root = tempfile::tempdir().unwrap();
    let expected = asset();
    let path = root.path().join(&expected.file);
    let neighbor = root.path().join("keep-original.nupkg");
    std::fs::write(&path, b"damaged cached package").unwrap();
    std::fs::write(&neighbor, b"original retained file").unwrap();
    discard_corrupt_cache_entry(root.path(), &expected).unwrap();
    assert!(!path.exists());
    assert_eq!(std::fs::read(&neighbor).unwrap(), b"original retained file");
    std::fs::write(&path, b"verified package").unwrap();
    discard_corrupt_cache_entry(root.path(), &expected).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"verified package");
}

#[test]
fn unexpected_directory_at_cache_filename_is_preserved() {
    let root = tempfile::tempdir().unwrap();
    let expected = asset();
    let path = root.path().join(&expected.file);
    std::fs::create_dir(&path).unwrap();
    std::fs::write(path.join("keep.txt"), b"keep").unwrap();
    assert!(discard_corrupt_cache_entry(root.path(), &expected).is_err());
    assert_eq!(std::fs::read(path.join("keep.txt")).unwrap(), b"keep");
}

#[test]
fn one_inflight_worker_keeps_repeated_check_and_download_admission_bounded() {
    let now = Instant::now();
    let backend = FakeBackend::new();
    let (release, gate) = mpsc::channel();
    *backend.check_gate.lock().unwrap() = Some(gate);
    let mut updates = controller(now, backend.clone());
    assert!(updates.check_now());
    for _ in 0..100 {
        assert!(!updates.check_now());
        assert!(!updates.download());
    }
    assert!(updates.prepare_ready().is_none());
    release.send(()).unwrap();
    finish(&mut updates, now);
    assert_eq!(backend.checks.load(Ordering::SeqCst), 1);
    assert!(matches!(updates.status(), Status::Available(_)));
}

#[test]
fn automatic_check_waits_for_startup_delay_and_idle_and_download_also_waits_for_idle() {
    let now = Instant::now();
    let backend = FakeBackend::new();
    let mut updates = controller(now, backend.clone());
    updates.poll(now + Duration::from_secs(29), true);
    updates.poll(now + Duration::from_secs(31), false);
    assert_eq!(backend.checks.load(Ordering::SeqCst), 0);
    updates.poll(now + Duration::from_secs(31), true);
    finish(&mut updates, now + Duration::from_secs(31));
    assert!(matches!(updates.status(), Status::Available(_)));
    updates.poll(now + Duration::from_secs(32), false);
    assert_eq!(backend.downloads.load(Ordering::SeqCst), 0);
    updates.poll(now + Duration::from_secs(32), true);
    finish(&mut updates, now + Duration::from_secs(33));
    assert!(updates.ready());
    assert_eq!(backend.downloads.load(Ordering::SeqCst), 1);
    updates.poll(now + CHECK_INTERVAL * 2, true);
    assert_eq!(
        backend.checks.load(Ordering::SeqCst),
        1,
        "ready package triggered another automatic check"
    );
    assert!(
        backend.applies.lock().unwrap().is_empty(),
        "download silently installed an update"
    );
}

#[test]
fn disabled_automatic_download_leaves_user_in_control_and_ready_token_does_not_apply() {
    let now = Instant::now();
    let backend = FakeBackend::new();
    let mut updates = controller(now, backend.clone());
    updates.settings.automatic_downloads = false;
    assert!(updates.check_now());
    finish(&mut updates, now);
    updates.poll(now + Duration::from_secs(60), true);
    assert_eq!(backend.downloads.load(Ordering::SeqCst), 0);
    assert!(updates.download());
    finish(&mut updates, now);
    let prepared = updates.prepare_ready().unwrap();
    let copied = prepared.clone();
    assert_eq!(copied.version(), "0.1.0-ci.2");
    assert!(backend.applies.lock().unwrap().is_empty());
    let data = tempfile::tempdir().unwrap();
    copied.apply_after_shutdown(false, data.path()).unwrap();
    assert_eq!(
        *backend.applies.lock().unwrap(),
        vec![(false, data.path().to_owned())]
    );
}

#[test]
fn failed_download_preserves_manual_retry_and_never_produces_apply_authority() {
    let now = Instant::now();
    let backend = FakeBackend::new();
    backend.fail_download.store(true, Ordering::SeqCst);
    let mut updates = controller(now, backend.clone());
    assert!(updates.check_now());
    finish(&mut updates, now);
    assert!(updates.download());
    finish(&mut updates, now);
    assert!(matches!(updates.status(), Status::Error(_)));
    assert!(updates.can_download());
    assert!(updates.prepare_ready().is_none());
    backend.fail_download.store(false, Ordering::SeqCst);
    assert!(updates.download());
    finish(&mut updates, now);
    assert!(updates.ready());
}

#[test]
fn offline_checks_back_off_but_explicit_retry_remains_available() {
    let now = Instant::now();
    let backend = FakeBackend::new();
    backend.fail_check.store(true, Ordering::SeqCst);
    let mut updates = controller(now, backend.clone());
    assert!(updates.check_now());
    finish(&mut updates, now);
    updates.poll(now + RETRY_DELAY - Duration::from_secs(1), true);
    assert_eq!(backend.checks.load(Ordering::SeqCst), 1);
    backend.fail_check.store(false, Ordering::SeqCst);
    assert!(updates.check_now());
    finish(&mut updates, now);
    assert!(matches!(updates.status(), Status::Available(_)));
}

#[test]
fn panicking_worker_retires_and_fresh_check_can_succeed() {
    let now = Instant::now();
    let backend = FakeBackend::new();
    backend.panic_check.store(true, Ordering::SeqCst);
    let mut updates = controller(now, backend);
    assert!(updates.check_now());
    finish(&mut updates, now);
    assert!(matches!(updates.status(), Status::Error(_)));
    assert!(updates.check_now());
    finish(&mut updates, now);
    assert!(matches!(updates.status(), Status::Available(_)));
}

#[test]
fn disconnected_worker_releases_admission_for_a_fresh_check() {
    let now = Instant::now();
    let mut updates = controller(now, FakeBackend::new());
    let (sender, receiver) = mpsc::sync_channel(1);
    let handle = std::thread::spawn(move || drop(sender));
    updates.worker = Some(Work {
        receiver,
        handle,
        result: None,
    });
    updates.status = Status::Checking;
    finish(&mut updates, now);
    assert!(matches!(updates.status(), Status::Error(_)));
    assert!(updates.check_now());
    finish(&mut updates, now);
    assert!(matches!(updates.status(), Status::Available(_)));
}

#[test]
fn rapid_completed_workers_do_not_lose_their_last_result_or_wedge_admission() {
    let now = Instant::now();
    let backend = FakeBackend::new();
    let mut updates = controller(now, backend.clone());
    for _ in 0..100 {
        assert!(updates.check_now());
        finish(&mut updates, now);
        assert!(matches!(updates.status(), Status::Available(_)));
    }
    assert_eq!(backend.checks.load(Ordering::SeqCst), 100);
}

#[test]
fn progress_notification_is_compared_to_last_reported_value() {
    let now = Instant::now();
    let mut updates = Updates::unmanaged(now);
    updates.progress.store(45, Ordering::Relaxed);
    assert!(updates.poll(now, false));
    assert!(!updates.poll(now, false));
    assert_eq!(updates.progress(), 45);
}

#[test]
fn source_build_and_render_fixture_have_no_network_or_install_authority() {
    let now = Instant::now();
    let mut updates = Updates::unmanaged(now);
    assert!(!updates.check_now());
    assert!(!updates.download());
    assert!(!updates.poll(now + CHECK_INTERVAL * 10, true));
    let mut fixture =
        Updates::presentation_fixture(Status::Ready(Release::from_asset(&asset())), now);
    assert!(fixture.installed());
    assert!(fixture.prepare_ready().is_none());
    assert!(!fixture.check_now());
    assert!(!fixture.download());
}

#[test]
fn legacy_preferences_receive_defaults_and_unknown_install_channel_is_not_used() {
    let settings: Settings = serde_json::from_str("{}").unwrap();
    assert!(settings.automatic_checks && settings.automatic_downloads && settings.install_on_quit);
    assert_eq!(Channel::installed("preview").unwrap(), Channel::Preview);
    assert_eq!(Channel::installed("stable").unwrap(), Channel::Stable);
    assert!(Channel::installed("arbitrary-feed").is_err());
    assert!(validate_data_root(Path::new("relative data")).is_err());
    let root = tempfile::tempdir().unwrap();
    validate_data_root(root.path()).unwrap();
    assert!(validate_data_root(&root.path().join("missing")).is_err());
}

#[test]
fn public_requests_are_fixed_and_have_no_installation_query() {
    for channel in [Channel::Preview, Channel::Stable] {
        let url = reqwest::Url::parse(&request_url(channel, "update.json")).unwrap();
        assert_eq!(url.host_str(), Some("github.com"));
        assert_eq!(url.query(), None);
        assert_eq!(url.fragment(), None);
        assert!(trusted_redirect(&url));
    }
    for url in [
        "http://github.com/a",
        "https://evil.example/a",
        "https://github.com.evil.example/a",
        "https://user@github.com/a",
        "https://github.com:444/a",
    ] {
        assert!(
            !trusted_redirect(&reqwest::Url::parse(url).unwrap()),
            "accepted {url}"
        );
    }
}

#[test]
fn bounded_catalog_and_marker_fail_closed() {
    assert!(read_bounded(&b"12345"[..], 4).is_err());
    let root = tempfile::tempdir().unwrap();
    assert!(load_marker(root.path()).unwrap().is_none());
    let path = root.path().join("kielbox-install.json");
    std::fs::write(
        &path,
        br#"{"schema_version":1,"appid":"Kielbox","channel":"preview","version":"0.1.0-ci.1"}"#,
    )
    .unwrap();
    assert_eq!(
        load_marker(root.path()).unwrap().unwrap().version,
        "0.1.0-ci.1"
    );
    for bytes in [
        b"invalid".to_vec(),
        vec![b' '; 4097],
        br#"{"schema_version":1,"appid":"Other","channel":"preview","version":"0.1.0"}"#.to_vec(),
    ] {
        std::fs::write(&path, bytes).unwrap();
        assert!(load_marker(root.path()).is_err());
    }
}

#[test]
fn local_feed_uses_the_same_catalog_stream_hash_and_cache_checks_as_production() {
    let root = tempfile::tempdir().unwrap();
    let feed = root.path().join("feed");
    std::fs::create_dir(&feed).unwrap();
    let expected = asset();
    std::fs::write(
        feed.join("update.json"),
        serde_json::to_vec(&expected).unwrap(),
    )
    .unwrap();
    std::fs::write(feed.join(&expected.file), b"verified package").unwrap();
    let backend = InstalledBackend {
        channel: Channel::Preview,
        version: semver::Version::parse("0.1.0-ci.1").unwrap(),
        packages: root.path().join("cache"),
        current_directory: root.path().to_path_buf(),
        source: Source::FileProof(feed.clone()),
    };
    let cancelled = AtomicBool::new(false);
    let CheckResult::Available(update) = backend.check(&cancelled).unwrap() else {
        panic!("valid newer installer was not offered");
    };
    backend
        .download(&update, Arc::new(AtomicU8::new(0)), &cancelled)
        .unwrap();
    verify_package(&backend.package_directory(&update), &update).unwrap();
    // A damaged cached installer must be replaced from the feed, never accepted
    // merely because the final filename already exists.
    std::fs::write(
        backend.package_directory(&update).join(&update.file),
        b"modified package",
    )
    .unwrap();
    backend
        .download(&update, Arc::new(AtomicU8::new(0)), &cancelled)
        .unwrap();
    verify_package(&backend.package_directory(&update), &update).unwrap();
    let mut foreign = expected;
    foreign.channel = "stable".into();
    std::fs::write(
        feed.join("update.json"),
        serde_json::to_vec(&foreign).unwrap(),
    )
    .unwrap();
    assert!(backend.check(&cancelled).is_err());
}

#[test]
fn shutdown_cancels_and_joins_the_owned_worker() {
    struct UntilCancelled {
        started: AtomicBool,
        retired: AtomicBool,
    }
    impl Backend for UntilCancelled {
        fn check(&self, cancelled: &AtomicBool) -> Result<CheckResult, String> {
            self.started.store(true, Ordering::SeqCst);
            while !cancelled.load(Ordering::Relaxed) {
                std::thread::yield_now();
            }
            self.retired.store(true, Ordering::SeqCst);
            Err("cancelled".into())
        }
        fn download(&self, _: &Asset, _: Arc<AtomicU8>, _: &AtomicBool) -> Result<Asset, String> {
            panic!("unexpected download")
        }
        fn apply(&self, _: &Asset, _: bool, _: &Path) -> Result<(), String> {
            panic!("unexpected installation")
        }
    }
    let backend = Arc::new(UntilCancelled {
        started: AtomicBool::new(false),
        retired: AtomicBool::new(false),
    });
    let mut updates = Updates::unmanaged(Instant::now());
    updates.backend = Some(backend.clone());
    assert!(updates.check_now());
    let deadline = Instant::now() + Duration::from_secs(2);
    while !backend.started.load(Ordering::SeqCst) {
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
    drop(updates);
    assert!(backend.retired.load(Ordering::SeqCst));
}

#[test]
fn bounded_download_rejects_corrupt_truncated_oversize_and_cancelled_without_final_file() {
    let root = tempfile::tempdir().unwrap();
    let expected = asset();
    for bytes in [
        b"modified package".as_slice(),
        b"truncated",
        b"verified package plus extra",
    ] {
        assert!(
            stage_download(
                bytes,
                root.path(),
                &expected,
                &AtomicU8::new(0),
                &AtomicBool::new(false)
            )
            .is_err()
        );
        assert!(!root.path().join(&expected.file).exists());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }
    assert!(
        stage_download(
            b"verified package".as_slice(),
            root.path(),
            &expected,
            &AtomicU8::new(0),
            &AtomicBool::new(true)
        )
        .is_err()
    );
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    stage_download(
        b"verified package".as_slice(),
        root.path(),
        &expected,
        &AtomicU8::new(0),
        &AtomicBool::new(false),
    )
    .unwrap();
    verify_package(root.path(), &expected).unwrap();
}

#[test]
fn installer_arguments_preserve_unicode_spaces_and_never_force_close_processes() {
    let data = Path::new(r"C:\Users\Zoë\My captures");
    let args = installer_arguments(true, data, Path::new(r"C:\Apps\Kielbox"));
    assert!(args.contains(&OsString::from("/RESTARTAPP")));
    assert!(args.contains(&OsString::from(
        r"/WORKINGDIRECTORY=C:\Users\Zoë\My captures"
    )));
    assert!(
        args.iter()
            .all(|arg| !arg.to_string_lossy().contains("FORCECLOSE"))
    );
    assert!(
        !installer_arguments(false, data, Path::new(r"C:\Apps\Kielbox"))
            .contains(&OsString::from("/RESTARTAPP"))
    );
}

#[test]
fn installed_process_filter_includes_companion_tools_and_normalizes_windows_spelling() {
    let root = Path::new(r"C:\Apps\Kielbox\current");
    for image in [
        r"C:\Apps\Kielbox\current\rcr.exe",
        r"c:/apps/KIELBOX/CURRENT/clean.exe",
        r"\\?\C:\Apps\Kielbox\current\helpers\worker.exe",
        r"C:\Apps\Kielbox\current\.\helpers\..\clean.exe",
    ] {
        assert!(
            image_in_installation(root, Path::new(image)),
            "missed installed process {image}"
        );
    }
    assert!(image_in_installation(
        Path::new(r"\\?\C:\Üsers\Zoë\Kielbox\current\"),
        Path::new(r"c:\üsers\ZOË\kielbox\CURRENT\clean.exe"),
    ));
    assert!(image_in_installation(
        Path::new(r"\\?\UNC\server\share\Kielbox\current"),
        Path::new(r"\\SERVER\SHARE\kielbox\CURRENT\clean.exe"),
    ));
}

#[test]
fn installed_process_filter_does_not_confuse_prefix_neighbors_or_other_installations() {
    let root = Path::new(r"C:\Apps\Kielbox\current");
    for image in [
        r"C:\Apps\Kielbox\current-old\clean.exe",
        r"C:\Apps\Kielbox\currently\rcr.exe",
        r"C:\Apps\Kielbox\packages\rcr.exe",
        r"D:\Apps\Kielbox\current\rcr.exe",
        r"C:\Other\Kielbox\current\rcr.exe",
        r"C:\Apps\Kielbox\current\..\shared\tool.exe",
        r"C:\Apps\Kielbox\current",
    ] {
        assert!(
            !image_in_installation(root, Path::new(image)),
            "misidentified outside process {image}"
        );
    }
    assert!(!image_in_installation(
        Path::new(r"\\server\share\Kielbox\current"),
        Path::new(r"\\server\share-other\Kielbox\current\rcr.exe"),
    ));
}
