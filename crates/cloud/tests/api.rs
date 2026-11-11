//! Non-upload API calls, auth handling and configuration rules.

mod common;

use std::time::Duration;

use cloud::{CloudClient, CloudConfig, CloudError, IdentityKind, ItemKind, ListQuery};
use common::{ID, SECRET, client, item_json, json_body, requests};
use serde_json::json;
use url::Url;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn cfg(url: &str) -> CloudConfig {
    CloudConfig {
        base_url: Url::parse(url).unwrap(),
        client_id: ID.into(),
        client_secret: SECRET.into(),
    }
}

#[test]
fn debug_output_hides_the_secret() {
    let config = cfg("https://cloud.example.com");
    let dbg = format!("{config:?}");
    assert!(!dbg.contains(SECRET), "{dbg}");
    assert!(dbg.contains(ID));
    let c = CloudClient::new(config).unwrap();
    assert!(!format!("{c:?}").contains(SECRET));
}

#[test]
fn base_url_must_be_https_except_for_loopback() {
    assert!(CloudClient::new(cfg("https://cloud.example.com")).is_ok());
    assert!(CloudClient::new(cfg("http://localhost:8787")).is_ok());
    assert!(CloudClient::new(cfg("http://127.0.0.1:8787")).is_ok());
    for bad in [
        "http://cloud.example.com",
        "ftp://cloud.example.com",
        "http://localhost.evil.com",
        "https://user:pw@cloud.example.com",
    ] {
        assert!(
            matches!(CloudClient::new(cfg(bad)), Err(CloudError::Config { .. })),
            "{bad}"
        );
    }
}

#[tokio::test]
async fn me_parses_identity_and_limits() {
    let api = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/me"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "identity":"svc-token","kind":"service",
            "limits":{"max_upload_bytes":100,"multipart_threshold":64,"part_size":32}})))
        .mount(&api)
        .await;
    let me = client(&api).me().await.unwrap();
    assert_eq!(me.identity, "svc-token");
    assert_eq!(me.kind, IdentityKind::Service);
    assert_eq!(me.limits.part_size, 32);
    let r = &requests(&api, "GET", "/api/me").await[0];
    assert_eq!(r.headers.get("cf-access-client-id").unwrap(), ID);
    assert_eq!(r.headers.get("cf-access-client-secret").unwrap(), SECRET);
}

#[tokio::test]
async fn access_login_redirect_maps_to_auth() {
    let api = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/me"))
        .respond_with(ResponseTemplate::new(302).insert_header(
            "Location",
            "https://team.cloudflareaccess.com/cdn-cgi/access/login/x",
        ))
        .mount(&api)
        .await;
    let err = client(&api).me().await.unwrap_err();
    match err {
        CloudError::Auth { message } => assert!(message.contains("service token"), "{message}"),
        other => panic!("{other:?}"),
    }
    // The redirect target was not followed, so the secret could not leak to it.
    assert_eq!(api.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn unauthorized_maps_to_auth_and_other_errors_keep_the_envelope() {
    let api = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/me"))
        .respond_with(
            ResponseTemplate::new(401)
                .set_body_json(json!({"error":{"code":"unauthorized","message":"bad jwt"}})),
        )
        .mount(&api)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/items/gone"))
        .respond_with(
            ResponseTemplate::new(404)
                .set_body_json(json!({"error":{"code":"not_found","message":"no such item"}})),
        )
        .mount(&api)
        .await;
    let c = client(&api);
    assert!(matches!(c.me().await, Err(CloudError::Auth { .. })));
    match c.delete_item("gone").await.unwrap_err() {
        CloudError::Http {
            status: 404,
            code,
            message,
        } => {
            assert_eq!(code.as_deref(), Some("not_found"));
            assert_eq!(message, "no such item");
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn list_encodes_query_and_parses_pages() {
    let api = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/items"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(
                json!({"items":[item_json("A"), item_json("B")],"next_cursor":"abc"}),
            ),
        )
        .mount(&api)
        .await;
    let page = client(&api)
        .list(&ListQuery {
            limit: Some(2),
            cursor: Some("c&d".into()),
            q: Some("100% a b".into()),
            kind: Some(ItemKind::Screenshot),
            favorite: true,
        })
        .await
        .unwrap();
    assert_eq!(page.items.len(), 2);
    assert_eq!(page.items[0].kind, ItemKind::Recording);
    assert_eq!(page.next_cursor.as_deref(), Some("abc"));
    let r = &requests(&api, "GET", "/api/items").await[0];
    let q: std::collections::HashMap<_, _> = r.url.query_pairs().into_owned().collect();
    assert_eq!(q["limit"], "2");
    assert_eq!(q["cursor"], "c&d");
    assert_eq!(q["q"], "100% a b");
    assert_eq!(q["kind"], "screenshot");
    assert_eq!(q["favorite"], "1");
}

#[tokio::test]
async fn share_create_revoke_and_item_delete() {
    let api = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/items/ITEM/shares"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "share":{"id":"S1","item_id":"ITEM","created_at":1,"expires_at":604_801_000,
                     "revoked_at":null,"allow_download":true,"view_count":0,"last_viewed_at":null},
            "url":"https://share.example.com/s/TOKEN"})))
        .mount(&api)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/shares/S1"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&api)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/items/ITEM"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&api)
        .await;
    let c = client(&api);
    let share = c
        .create_share("ITEM", Some(Duration::from_hours(168)))
        .await
        .unwrap();
    assert_eq!(share.id, "S1");
    assert_eq!(
        share.url.as_deref(),
        Some("https://share.example.com/s/TOKEN")
    );
    assert_eq!(share.expires_at, Some(604_801_000));
    let sent = json_body(&requests(&api, "POST", "/api/items/ITEM/shares").await[0]);
    assert_eq!(sent, json!({"expires_in_seconds": 604_800}));
    c.create_share("ITEM", None).await.unwrap();
    let sent = json_body(&requests(&api, "POST", "/api/items/ITEM/shares").await[1]);
    assert_eq!(sent, json!({"expires_in_seconds": null}));
    c.revoke_share("S1").await.unwrap();
    c.delete_item("ITEM").await.unwrap();
}
