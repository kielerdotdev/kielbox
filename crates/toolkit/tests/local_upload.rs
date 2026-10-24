//! Compile the actual vendored local uploader and its regression tests against
//! RCR's dependency lockfile. The upstream workspace also resolves unrelated
//! applications and its own, independently versioned development dependencies.
pub use ssx_upload::{body, context, error, http, types, util};

#[path = "../../../vendor/ssx/crates/ssx-upload/src/local.rs"]
mod local;

#[tokio::test]
async fn shared_folder_upload_preserves_existing_destination_and_source() {
    use ssx_upload::{UploadContext, UploadKind, UploadRequest, Uploader};
    let directory = tempfile::tempdir().unwrap();
    let original = directory.path().join("source.png");
    let shared = directory.path().join("shared");
    std::fs::create_dir(&shared).unwrap();
    std::fs::write(&original, b"source image").unwrap();
    std::fs::write(shared.join("source.png"), b"previous upload").unwrap();
    let uploader =
        local::LocalUploader::to_directory(&shared).with_base_url("https://example.invalid");
    let result = uploader
        .upload(
            &UploadRequest::from_path(&original, UploadKind::Image),
            &UploadContext::new(ssx_upload::build_http_client().unwrap()),
        )
        .await
        .unwrap();
    assert_eq!(result.url, "https://example.invalid/source-1.png");
    assert_eq!(
        std::fs::read(shared.join("source.png")).unwrap(),
        b"previous upload"
    );
    assert_eq!(
        std::fs::read(shared.join("source-1.png")).unwrap(),
        b"source image"
    );
    assert_eq!(std::fs::read(&original).unwrap(), b"source image");
    let _ = local::LocalUploader::noop();
}
