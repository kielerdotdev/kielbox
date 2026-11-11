//! Upload behaviour against wiremock: single PUT, multipart, retries, cancellation.

mod common;

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use cloud::CloudError;
use common::{
    ID, Recorder, SECRET, client, json_body, mock_abort, mock_complete, mock_create, new_upload,
    requests, sparse_file,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;
use upload::NullProgress;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

fn part_urls(base: &str, prefix: &str, numbers: &[u32]) -> serde_json::Value {
    json!({ "parts": numbers.iter().map(|n| json!({
        "part_number": n, "url": format!("{base}/{prefix}/{n}"), "expires_at": 0
    })).collect::<Vec<_>>() })
}

async fn mock_part_put(storage: &MockServer, n: u32) {
    Mock::given(method("PUT"))
        .and(path(format!("/p/{n}")))
        .respond_with(ResponseTemplate::new(200).insert_header("ETag", format!("\"etag-{n}\"")))
        .mount(storage)
        .await;
}

#[tokio::test]
async fn single_put_streams_the_file_with_the_presigned_headers() {
    let api = MockServer::start().await;
    let storage = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("clip.mp4");
    let data: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
    std::fs::write(&file, &data).unwrap();

    mock_create(
        &api,
        "ITEM1",
        json!({"mode":"single","url":format!("{}/put/orig?sig=abc", storage.uri()),
               "headers":{"content-type":"video/mp4"},"expires_at":1}),
        None,
    )
    .await;
    mock_complete(&api, "ITEM1").await;
    Mock::given(method("PUT"))
        .and(path("/put/orig"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&storage)
        .await;

    let rec = Recorder::new();
    let item = client(&api)
        .upload(new_upload(file), rec.clone(), CancellationToken::new())
        .await
        .expect("upload");
    assert_eq!(item.id, "ITEM1");
    assert_eq!(item.title, "My clip");

    let create = &requests(&api, "POST", "/api/uploads").await[0];
    let body = json_body(create);
    assert_eq!(body["filename"], "clip.mp4");
    assert_eq!(body["size"], 300_000);
    assert_eq!(body["content_type"], "video/mp4");
    assert_eq!(body["kind"], "recording");
    assert_eq!(body["title"], "My clip");
    assert_eq!(body["captured_at"], 1_700_000_000_000_i64);
    assert_eq!(body["width"], 1920);
    assert_eq!(body["duration_ms"], 4200);
    assert_eq!(body["thumbnail"], false);
    assert_eq!(create.headers.get("cf-access-client-id").unwrap(), ID);
    assert_eq!(
        create.headers.get("cf-access-client-secret").unwrap(),
        SECRET
    );
    assert_eq!(
        create.headers.get("content-type").unwrap(),
        "application/json"
    );

    let put = &requests(&storage, "PUT", "/put/orig").await[0];
    assert_eq!(put.headers.get("content-type").unwrap(), "video/mp4");
    assert_eq!(put.headers.get("content-length").unwrap(), "300000");
    assert!(put.headers.get("transfer-encoding").is_none());
    assert_eq!(put.body, data);
    assert!(put.url.query().is_some_and(|q| q.contains("sig=abc")));

    // Single PUT completes without a parts list.
    let complete = &requests(&api, "POST", "/api/uploads/ITEM1/complete").await[0];
    assert_eq!(json_body(complete), json!({}));

    let ev = rec.events();
    assert!(ev.windows(2).all(|w| w[0].0 <= w[1].0));
    assert_eq!(ev.last().copied(), Some((300_000, Some(300_000))));
}

#[tokio::test]
async fn multipart_streams_exact_parts_and_round_trips_etags() {
    const MIB: u64 = 1024 * 1024;
    let api = MockServer::start().await;
    let storage = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    // 20 MiB sparse file, 8 MiB parts: 8 + 8 + 4.
    let file = sparse_file(dir.path(), "big.mp4", 20 * MIB);

    mock_create(
        &api,
        "ITEM2",
        json!({"mode":"multipart","part_size":8 * MIB,"part_count":3}),
        None,
    )
    .await;
    Mock::given(method("POST"))
        .and(path("/api/uploads/ITEM2/parts"))
        .respond_with(ResponseTemplate::new(200).set_body_json(part_urls(
            &storage.uri(),
            "p",
            &[1, 2, 3],
        )))
        .mount(&api)
        .await;
    mock_complete(&api, "ITEM2").await;
    for n in 1..=3 {
        mock_part_put(&storage, n).await;
    }

    let rec = Recorder::new();
    client(&api)
        .upload(new_upload(file), rec.clone(), CancellationToken::new())
        .await
        .expect("upload");

    let lens: Vec<usize> = {
        let mut v = Vec::new();
        for n in 1..=3 {
            let r = &requests(&storage, "PUT", &format!("/p/{n}")).await[0];
            assert_eq!(
                r.headers.get("content-length").unwrap().to_str().unwrap(),
                r.body.len().to_string()
            );
            assert!(r.headers.get("cf-access-client-id").is_none());
            v.push(r.body.len());
        }
        v
    };
    assert_eq!(
        lens,
        vec![(8 * MIB) as usize, (8 * MIB) as usize, (4 * MIB) as usize]
    );

    // One batch request covers all three parts.
    let parts_reqs = requests(&api, "POST", "/api/uploads/ITEM2/parts").await;
    assert_eq!(parts_reqs.len(), 1);
    assert_eq!(json_body(&parts_reqs[0]), json!({"part_numbers":[1,2,3]}));

    let complete = &requests(&api, "POST", "/api/uploads/ITEM2/complete").await[0];
    assert_eq!(
        json_body(complete),
        json!({"parts":[
            {"part_number":1,"etag":"\"etag-1\""},
            {"part_number":2,"etag":"\"etag-2\""},
            {"part_number":3,"etag":"\"etag-3\""}]})
    );

    let ev = rec.events();
    assert!(
        ev.windows(2).all(|w| w[0].0 <= w[1].0),
        "progress went backwards"
    );
    assert_eq!(ev.last().copied(), Some((20 * MIB, Some(20 * MIB))));
}

#[tokio::test]
async fn part_urls_are_requested_eight_at_a_time() {
    let api = MockServer::start().await;
    let storage = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    // 10 parts of 1 KiB.
    let file = sparse_file(dir.path(), "x.mp4", 10 * 1024);
    mock_create(
        &api,
        "ITEM9",
        json!({"mode":"multipart","part_size":1024,"part_count":10}),
        None,
    )
    .await;
    let base = storage.uri();
    Mock::given(method("POST"))
        .and(path("/api/uploads/ITEM9/parts"))
        .respond_with(move |req: &Request| {
            let nums: Vec<u32> =
                serde_json::from_slice::<serde_json::Value>(&req.body).unwrap()["part_numbers"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_u64().unwrap() as u32)
                    .collect();
            ResponseTemplate::new(200).set_body_json(part_urls(&base, "p", &nums))
        })
        .mount(&api)
        .await;
    mock_complete(&api, "ITEM9").await;
    for n in 1..=10 {
        mock_part_put(&storage, n).await;
    }
    client(&api)
        .upload(
            new_upload(file),
            Arc::new(NullProgress),
            CancellationToken::new(),
        )
        .await
        .expect("upload");
    let reqs = requests(&api, "POST", "/api/uploads/ITEM9/parts").await;
    let batches: Vec<_> = reqs
        .iter()
        .map(|r| json_body(r)["part_numbers"].clone())
        .collect();
    assert_eq!(
        batches,
        vec![json!([1, 2, 3, 4, 5, 6, 7, 8]), json!([9, 10])]
    );
}

#[tokio::test]
async fn failed_part_is_retried_without_progress_going_backwards() {
    const KIB: u64 = 1024;
    let api = MockServer::start().await;
    let storage = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let file = sparse_file(dir.path(), "r.mp4", 300 * KIB);
    mock_create(
        &api,
        "ITEM3",
        json!({"mode":"multipart","part_size":100 * KIB,"part_count":3}),
        None,
    )
    .await;
    Mock::given(method("POST"))
        .and(path("/api/uploads/ITEM3/parts"))
        .respond_with(ResponseTemplate::new(200).set_body_json(part_urls(
            &storage.uri(),
            "p",
            &[1, 2, 3],
        )))
        .mount(&api)
        .await;
    mock_complete(&api, "ITEM3").await;
    mock_part_put(&storage, 1).await;
    mock_part_put(&storage, 3).await;
    Mock::given(method("PUT"))
        .and(path("/p/2"))
        .respond_with(ResponseTemplate::new(500))
        .up_to_n_times(2)
        .mount(&storage)
        .await;
    mock_part_put(&storage, 2).await;

    let rec = Recorder::new();
    client(&api)
        .upload(new_upload(file), rec.clone(), CancellationToken::new())
        .await
        .expect("upload");
    assert_eq!(
        requests(&storage, "PUT", "/p/2").await.len(),
        3,
        "two 500s then success"
    );
    let ev = rec.events();
    assert!(
        ev.windows(2).all(|w| w[0].0 <= w[1].0),
        "progress went backwards: {ev:?}"
    );
    assert_eq!(ev.last().unwrap().0, 300 * KIB);
    let complete = &requests(&api, "POST", "/api/uploads/ITEM3/complete").await[0];
    assert_eq!(json_body(complete)["parts"][1]["etag"], "\"etag-2\"");
}

#[tokio::test]
async fn part_gives_up_after_four_retries_and_aborts_the_upload() {
    let api = MockServer::start().await;
    let storage = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let file = sparse_file(dir.path(), "g.mp4", 2048);
    mock_create(
        &api,
        "ITEMG",
        json!({"mode":"multipart","part_size":1024,"part_count":2}),
        None,
    )
    .await;
    Mock::given(method("POST"))
        .and(path("/api/uploads/ITEMG/parts"))
        .respond_with(ResponseTemplate::new(200).set_body_json(part_urls(
            &storage.uri(),
            "p",
            &[1, 2],
        )))
        .mount(&api)
        .await;
    mock_abort(&api, "ITEMG").await;
    Mock::given(method("PUT"))
        .and(path("/p/1"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&storage)
        .await;

    let err = client(&api)
        .upload(
            new_upload(file),
            Arc::new(NullProgress),
            CancellationToken::new(),
        )
        .await
        .expect_err("must fail");
    assert!(
        matches!(err, CloudError::Http { status: 503, .. }),
        "{err:?}"
    );
    assert_eq!(
        requests(&storage, "PUT", "/p/1").await.len(),
        5,
        "1 try + 4 retries"
    );
    assert_eq!(
        requests(&api, "DELETE", "/api/uploads/ITEMG").await.len(),
        1
    );
}

#[tokio::test]
async fn forbidden_part_url_is_replaced_with_a_fresh_one() {
    let api = MockServer::start().await;
    let storage = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let file = sparse_file(dir.path(), "f.mp4", 2048);
    mock_create(
        &api,
        "ITEM4",
        json!({"mode":"multipart","part_size":1024,"part_count":2}),
        None,
    )
    .await;
    // First batch hands out an expired URL for part 1; the second batch fixes it.
    let old = json!({"parts":[
        {"part_number":1,"url":format!("{}/old/1", storage.uri()),"expires_at":0},
        {"part_number":2,"url":format!("{}/p/2", storage.uri()),"expires_at":0}]});
    Mock::given(method("POST"))
        .and(path("/api/uploads/ITEM4/parts"))
        .respond_with(ResponseTemplate::new(200).set_body_json(old))
        .up_to_n_times(1)
        .mount(&api)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/uploads/ITEM4/parts"))
        .respond_with(ResponseTemplate::new(200).set_body_json(part_urls(
            &storage.uri(),
            "p",
            &[1, 2],
        )))
        .mount(&api)
        .await;
    mock_complete(&api, "ITEM4").await;
    Mock::given(method("PUT"))
        .and(path("/old/1"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&storage)
        .await;
    mock_part_put(&storage, 1).await;
    mock_part_put(&storage, 2).await;

    client(&api)
        .upload(
            new_upload(file),
            Arc::new(NullProgress),
            CancellationToken::new(),
        )
        .await
        .expect("upload");
    assert_eq!(requests(&storage, "PUT", "/old/1").await.len(), 1);
    assert_eq!(requests(&storage, "PUT", "/p/1").await.len(), 1);
    assert_eq!(
        requests(&api, "POST", "/api/uploads/ITEM4/parts")
            .await
            .len(),
        2
    );
}

#[tokio::test]
async fn cancelling_mid_part_calls_the_abort_endpoint() {
    let api = MockServer::start().await;
    let storage = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let file = sparse_file(dir.path(), "c.mp4", 2048);
    mock_create(
        &api,
        "ITEM5",
        json!({"mode":"multipart","part_size":1024,"part_count":2}),
        None,
    )
    .await;
    Mock::given(method("POST"))
        .and(path("/api/uploads/ITEM5/parts"))
        .respond_with(ResponseTemplate::new(200).set_body_json(part_urls(
            &storage.uri(),
            "p",
            &[1, 2],
        )))
        .mount(&api)
        .await;
    mock_abort(&api, "ITEM5").await;
    Mock::given(method("PUT"))
        .and(path("/p/1"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(30)))
        .mount(&storage)
        .await;

    let cancel = CancellationToken::new();
    let c = client(&api);
    let task = tokio::spawn({
        let cancel = cancel.clone();
        async move {
            c.upload(new_upload(file), Arc::new(NullProgress), cancel)
                .await
        }
    });
    // Wait until the part PUT is in flight.
    for _ in 0..200 {
        if !requests(&storage, "PUT", "/p/1").await.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        !requests(&storage, "PUT", "/p/1").await.is_empty(),
        "part never started"
    );
    cancel.cancel();
    let err = task.await.unwrap().expect_err("cancelled");
    assert!(err.is_cancelled(), "{err:?}");
    assert_eq!(
        requests(&api, "DELETE", "/api/uploads/ITEM5").await.len(),
        1
    );
    assert!(
        requests(&api, "POST", "/api/uploads/ITEM5/complete")
            .await
            .is_empty()
    );
}

struct Truncate(std::path::PathBuf, String);
impl Respond for Truncate {
    fn respond(&self, _: &Request) -> ResponseTemplate {
        std::fs::OpenOptions::new()
            .write(true)
            .open(&self.0)
            .unwrap()
            .set_len(1000)
            .unwrap();
        ResponseTemplate::new(200).set_body_json(part_urls(&self.1, "p", &[1, 2]))
    }
}

#[tokio::test]
async fn shrinking_source_is_reported_as_source_changed() {
    let api = MockServer::start().await;
    let storage = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let file = sparse_file(dir.path(), "s.mp4", 4096);
    mock_create(
        &api,
        "ITEM6",
        json!({"mode":"multipart","part_size":2048,"part_count":2}),
        None,
    )
    .await;
    mock_abort(&api, "ITEM6").await;
    // The file is truncated (still being written / replaced) after the stat but before
    // the first part is streamed.
    Mock::given(method("POST"))
        .and(path("/api/uploads/ITEM6/parts"))
        .respond_with(Truncate(file.clone(), storage.uri()))
        .mount(&api)
        .await;
    mock_part_put(&storage, 1).await;
    mock_part_put(&storage, 2).await;

    let err = client(&api)
        .upload(
            new_upload(file),
            Arc::new(NullProgress),
            CancellationToken::new(),
        )
        .await
        .expect_err("must fail");
    assert!(matches!(err, CloudError::SourceChanged), "{err:?}");
    assert_eq!(
        requests(&api, "DELETE", "/api/uploads/ITEM6").await.len(),
        1
    );
}

#[tokio::test]
async fn thumbnail_is_uploaded_before_complete_and_its_failure_is_ignored() {
    for thumb_status in [200_u16, 500] {
        let api = MockServer::start().await;
        let storage = MockServer::start().await;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("t.png");
        std::fs::write(&file, b"png-bytes").unwrap();
        mock_create(
            &api,
            "ITEM7",
            json!({"mode":"single","url":format!("{}/put/orig", storage.uri()),"headers":{}}),
            Some(
                json!({"mode":"single","url":format!("{}/put/thumb", storage.uri()),
                        "headers":{"content-type":"image/jpeg"}}),
            ),
        )
        .await;
        mock_complete(&api, "ITEM7").await;
        Mock::given(method("PUT"))
            .and(path("/put/orig"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&storage)
            .await;
        Mock::given(method("PUT"))
            .and(path("/put/thumb"))
            .respond_with(ResponseTemplate::new(thumb_status))
            .mount(&storage)
            .await;

        let mut up = new_upload(file);
        up.thumbnail_jpeg = Some(Bytes::from_static(b"\xff\xd8jpeg"));
        client(&api)
            .upload(up, Arc::new(NullProgress), CancellationToken::new())
            .await
            .expect("upload succeeds regardless of the thumbnail");

        let create = &requests(&api, "POST", "/api/uploads").await[0];
        assert_eq!(json_body(create)["thumbnail"], true);
        let orig = &requests(&storage, "PUT", "/put/orig").await[0];
        assert_eq!(orig.headers.get("content-type").unwrap(), "image/png");
        let thumb = &requests(&storage, "PUT", "/put/thumb").await[0];
        assert_eq!(thumb.body, b"\xff\xd8jpeg");
        assert_eq!(thumb.headers.get("content-type").unwrap(), "image/jpeg");
        assert!(thumb.headers.get("cf-access-client-id").is_none());
        assert!(
            requests(&api, "DELETE", "/api/uploads/ITEM7")
                .await
                .is_empty()
        );
    }
}

#[tokio::test]
async fn credentials_never_reach_the_presigned_host() {
    let api = MockServer::start().await;
    let storage = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("a.png");
    std::fs::write(&file, b"x").unwrap();
    mock_create(
        &api,
        "ITEM8",
        json!({"mode":"single","url":format!("{}/put/orig", storage.uri()),"headers":{}}),
        None,
    )
    .await;
    mock_complete(&api, "ITEM8").await;
    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&storage)
        .await;

    client(&api)
        .with_dev_identity("tester")
        .upload(
            new_upload(file),
            Arc::new(NullProgress),
            CancellationToken::new(),
        )
        .await
        .expect("upload");

    let seen = storage.received_requests().await.unwrap();
    assert!(!seen.is_empty());
    for r in seen {
        for (name, value) in &r.headers {
            let n = name.as_str().to_ascii_lowercase();
            assert!(
                !n.starts_with("cf-access") && n != "x-dev-identity" && n != "authorization",
                "{n}"
            );
            assert!(!value.to_str().unwrap_or("").contains(SECRET));
        }
    }
    // ... while the API did get them.
    let api_req = &requests(&api, "POST", "/api/uploads").await[0];
    assert_eq!(api_req.headers.get("x-dev-identity").unwrap(), "tester");
}

#[tokio::test]
async fn failed_create_does_not_try_to_abort() {
    let api = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/uploads"))
        .respond_with(
            ResponseTemplate::new(415)
                .set_body_json(json!({"error":{"code":"unsupported_type","message":"nope"}})),
        )
        .mount(&api)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("a.xyz");
    std::fs::write(&file, b"x").unwrap();
    let err = client(&api)
        .upload(
            new_upload(file),
            Arc::new(NullProgress),
            CancellationToken::new(),
        )
        .await
        .expect_err("fails");
    match err {
        CloudError::Http {
            status: 415, code, ..
        } => assert_eq!(code.as_deref(), Some("unsupported_type")),
        other => panic!("{other:?}"),
    }
    assert_eq!(
        api.received_requests().await.unwrap().len(),
        1,
        "only the create call"
    );
}
