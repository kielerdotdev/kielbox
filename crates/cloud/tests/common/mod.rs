//! Shared helpers for the wiremock tests.
#![allow(
    dead_code,
    reason = "each test binary uses a different subset of the helpers"
)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cloud::{CloudClient, CloudConfig, ItemKind, NewUpload};
use serde_json::{Value, json};
use upload::ProgressSink;
use url::Url;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

pub const ID: &str = "client-id-123";
pub const SECRET: &str = "super-secret-value";

/// Client for a mock server: proxies ignored, tiny retry backoff.
pub fn client(api: &MockServer) -> CloudClient {
    CloudClient::new(CloudConfig {
        base_url: Url::parse(&api.uri()).expect("url"),
        client_id: ID.into(),
        client_secret: SECRET.into(),
    })
    .expect("client")
    .without_proxy()
    .expect("no proxy")
    .with_retry_backoff(Duration::from_millis(5))
}

/// Progress sink that records every update.
#[derive(Default)]
pub struct Recorder(pub Mutex<Vec<(u64, Option<u64>)>>);

impl Recorder {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn events(&self) -> Vec<(u64, Option<u64>)> {
        self.0.lock().expect("lock").clone()
    }
}

impl ProgressSink for Recorder {
    fn report(&self, sent: u64, total: Option<u64>) {
        self.0.lock().expect("lock").push((sent, total));
    }
}

pub fn sparse_file(dir: &Path, name: &str, len: u64) -> PathBuf {
    let p = dir.join(name);
    let f = std::fs::File::create(&p).expect("create");
    f.set_len(len).expect("set_len");
    p
}

pub fn new_upload(path: PathBuf) -> NewUpload {
    NewUpload {
        path,
        kind: ItemKind::Recording,
        title: Some("My clip".into()),
        captured_at_ms: 1_700_000_000_000,
        width: Some(1920),
        height: Some(1080),
        duration_ms: Some(4200),
        thumbnail_jpeg: None,
    }
}

pub fn item_json(id: &str) -> Value {
    json!({
        "id": id, "status": "ready", "kind": "recording", "title": "My clip",
        "filename": "clip.mp4", "content_type": "video/mp4", "size": 10,
        "captured_at": 1_700_000_000_000_i64, "created_at": 1_700_000_001_000_i64,
        "created_by": "svc", "favorite": false,
        "media_url": format!("/api/items/{id}/media"), "thumb_url": null
    })
}

/// `POST /api/uploads` answering with `upload` (and optionally a thumbnail target).
pub async fn mock_create(api: &MockServer, item_id: &str, upload: Value, thumb: Option<Value>) {
    let mut body = json!({ "item_id": item_id, "upload": upload });
    if let Some(t) = thumb {
        body["thumbnail_upload"] = t;
    }
    Mock::given(method("POST"))
        .and(path("/api/uploads"))
        .respond_with(ResponseTemplate::new(201).set_body_json(body))
        .mount(api)
        .await;
}

pub async fn mock_complete(api: &MockServer, item_id: &str) {
    Mock::given(method("POST"))
        .and(path(format!("/api/uploads/{item_id}/complete")))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "item": item_json(item_id) })),
        )
        .mount(api)
        .await;
}

pub async fn mock_abort(api: &MockServer, item_id: &str) {
    Mock::given(method("DELETE"))
        .and(path(format!("/api/uploads/{item_id}")))
        .respond_with(ResponseTemplate::new(204))
        .mount(api)
        .await;
}

pub fn json_body(req: &Request) -> Value {
    serde_json::from_slice(&req.body).expect("json body")
}

/// All recorded requests with this method and path.
pub async fn requests(server: &MockServer, m: &str, p: &str) -> Vec<Request> {
    server
        .received_requests()
        .await
        .expect("recording enabled")
        .into_iter()
        .filter(|r| r.method.as_str() == m && r.url.path() == p)
        .collect()
}
