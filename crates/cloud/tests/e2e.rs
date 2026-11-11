//! End-to-end test against a running server (`npm run dev` in `cloud/`).
//!
//! ```text
//! KIELBOX_CLOUD_TEST_URL=http://localhost:8787 \
//!   cargo test -p cloud --test e2e -- --ignored --nocapture
//! ```
//!
//! The server must run with `ENVIRONMENT=dev` so that `X-Dev-Identity` replaces Access, and
//! with the local signer, so presigned URLs point back at the Worker. Uploads a 70 MiB
//! sparse file (three multipart parts at the server's part size), then reads a range that
//! straddles a part boundary back through `/api/items/:id/media`.

mod common;

use std::sync::Arc;

use cloud::{CloudClient, CloudConfig};
use common::{new_upload, sparse_file};
use tokio_util::sync::CancellationToken;
use upload::NullProgress;
use url::Url;

const SIZE: u64 = 70 * 1024 * 1024;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs a running Kielbox Cloud server: set KIELBOX_CLOUD_TEST_URL"]
async fn seventy_mib_multipart_upload_reads_back_by_range() {
    let url = std::env::var("KIELBOX_CLOUD_TEST_URL").expect("KIELBOX_CLOUD_TEST_URL");
    let base = Url::parse(&url).expect("valid URL");
    let client = CloudClient::new(CloudConfig {
        base_url: base.clone(),
        client_id: "dev".into(),
        client_secret: "dev".into(),
    })
    .expect("client")
    .without_proxy()
    .expect("no proxy")
    .with_dev_identity("e2e@example.com");

    let me = client.me().await.expect("/api/me");
    assert!(
        SIZE >= me.limits.multipart_threshold,
        "file must exceed the multipart threshold"
    );

    let dir = tempfile::tempdir().expect("tempdir");
    let file = sparse_file(dir.path(), "e2e.mp4", SIZE);
    let item = client
        .upload(
            new_upload(file),
            Arc::new(NullProgress),
            CancellationToken::new(),
        )
        .await
        .expect("upload");
    assert_eq!(item.size, SIZE);
    assert_eq!(item.status, "ready");

    // Read 32 bytes across the first 32 MiB part boundary.
    let start = 32 * 1024 * 1024 - 16;
    let http = upload::http::client_builder()
        .expect("builder")
        .no_proxy()
        .build()
        .expect("http");
    let media = base
        .join(item.media_url.trim_start_matches('/'))
        .expect("media url");
    let resp = http
        .get(media)
        .header("x-dev-identity", "e2e@example.com")
        .header("range", format!("bytes={start}-{}", start + 31))
        .send()
        .await
        .expect("range request");
    assert_eq!(resp.status().as_u16(), 206);
    assert_eq!(
        resp.headers()
            .get("content-range")
            .and_then(|v| v.to_str().ok()),
        Some(format!("bytes {start}-{}/{SIZE}", start + 31).as_str())
    );
    let bytes = resp.bytes().await.expect("body");
    assert_eq!(bytes.as_ref(), &[0u8; 32]);

    client.delete_item(&item.id).await.expect("delete");
}
