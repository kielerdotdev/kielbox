//! [`CloudClient`]: the API calls and the upload state machine.

use std::fmt::{self, Write as _};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use bytes::{Bytes, BytesMut};
use reqwest::header::{CONTENT_LENGTH, CONTENT_TYPE, ETAG, HeaderName, HeaderValue};
use reqwest::{Method, StatusCode};
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio_util::sync::CancellationToken;
use upload::body::{BodyPlan, Payload};
use upload::{ProgressSink, ThrottledProgress, UploadContext, UploadError};
use url::Url;

use crate::body::{PartSink, ranged_body};
use crate::error::CloudError;
use crate::model::{
    CloudConfig, CompleteRequest, CompletedPart, CreateShareRequest, CreateShareResponse,
    CreateUploadRequest, CreateUploadResponse, Item, ItemEnvelope, ListQuery, Me, NewUpload, Page,
    PartsRequest, PartsResponse, Share, UploadTarget,
};

/// Part URLs are requested this many at a time.
const PART_URL_BATCH: u32 = 8;
/// Retries per part after the first attempt.
const PART_RETRIES: u32 = 4;
/// Upper bound for the best-effort abort call.
const ABORT_TIMEOUT: Duration = Duration::from_secs(10);
/// Largest API response body we buffer.
const MAX_API_BODY: usize = 8 * 1024 * 1024;
/// Progress callbacks are spaced at least this far apart.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(50);
/// S3 allows at most this many parts.
const MAX_PARTS: u32 = 10_000;

/// Client for one Kielbox Cloud deployment. Cheap to clone.
#[derive(Clone)]
pub struct CloudClient {
    /// Talks to the API origin only; redirects are not followed.
    api: reqwest::Client,
    /// Talks to presigned storage URLs; never carries credentials.
    storage: reqwest::Client,
    base: Url,
    client_id: String,
    client_secret: String,
    extra_headers: Vec<(HeaderName, HeaderValue)>,
    backoff: Duration,
}

impl fmt::Debug for CloudClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CloudClient")
            .field("base", &self.base.as_str())
            .field("client_id", &self.client_id)
            .field("client_secret", &"<redacted>")
            .finish_non_exhaustive()
    }
}

fn build_clients(no_proxy: bool) -> Result<(reqwest::Client, reqwest::Client), CloudError> {
    let build = || {
        let mut b = upload::http::client_builder()
            .map_err(|e| CloudError::config(e.to_string()))?
            .redirect(reqwest::redirect::Policy::none());
        if no_proxy {
            b = b.no_proxy();
        }
        b.build()
            .map_err(|e| CloudError::config(format!("could not build HTTP client: {e}")))
    };
    Ok((build()?, build()?))
}

fn validate_base(url: &Url) -> Result<(), CloudError> {
    let local = matches!(url.host_str(), Some("localhost" | "127.0.0.1"));
    match url.scheme() {
        "https" => {}
        "http" if local => {}
        "http" => {
            return Err(CloudError::config(
                "the cloud URL must use https (http is only allowed for localhost)",
            ));
        }
        other => {
            return Err(CloudError::config(format!(
                "unsupported URL scheme '{other}'"
            )));
        }
    }
    if url.host_str().is_none() {
        return Err(CloudError::config("the cloud URL has no host"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(CloudError::config(
            "the cloud URL must not contain credentials",
        ));
    }
    Ok(())
}

impl CloudClient {
    /// Validate `config` and build the HTTP clients.
    pub fn new(config: CloudConfig) -> Result<Self, CloudError> {
        validate_base(&config.base_url)?;
        let mut base = config.base_url;
        base.set_query(None);
        base.set_fragment(None);
        if !base.path().ends_with('/') {
            let p = format!("{}/", base.path());
            base.set_path(&p);
        }
        let (api, storage) = build_clients(false)?;
        Ok(Self {
            api,
            storage,
            base,
            client_id: config.client_id,
            client_secret: config.client_secret,
            extra_headers: Vec::new(),
            backoff: Duration::from_millis(500),
        })
    }

    /// `GET /api/me`: who the credentials belong to and the server limits. This is the
    /// "test connection" call.
    pub async fn me(&self) -> Result<Me, CloudError> {
        self.json(Method::GET, "api/me", None::<&()>, None).await
    }

    /// Upload `new` and return the finished item.
    ///
    /// Files at or above the server's multipart threshold go up as sequential presigned
    /// parts, smaller ones as one `PUT`. `progress` receives monotonic `(sent, total)`
    /// counts over the whole file, also across retries. On failure or cancellation a
    /// best-effort `DELETE /api/uploads/:id` (at most 10 s) removes the half-finished item.
    pub async fn upload(
        &self,
        new: NewUpload,
        progress: Arc<dyn ProgressSink>,
        cancel: CancellationToken,
    ) -> Result<Item, CloudError> {
        let meta = tokio::fs::metadata(&new.path)
            .await
            .map_err(|e| CloudError::io(format!("reading {}", new.path.display()), e))?;
        if !meta.is_file() || meta.len() == 0 {
            return Err(CloudError::config(
                "only non-empty regular files can be uploaded",
            ));
        }
        let size = meta.len();
        let filename = new
            .path
            .file_name()
            .map_or_else(|| "file".to_owned(), |n| n.to_string_lossy().into_owned());
        let content_type = content_type_for(&new.path);
        let req = CreateUploadRequest {
            filename: &filename,
            size,
            content_type,
            kind: new.kind,
            title: new.title.as_deref(),
            captured_at: new.captured_at_ms,
            width: new.width,
            height: new.height,
            duration_ms: new.duration_ms,
            thumbnail: new.thumbnail_jpeg.is_some(),
        };
        let created: CreateUploadResponse = self
            .json(Method::POST, "api/uploads", Some(&req), Some(&cancel))
            .await?;
        let id = created.item_id.clone();
        let progress: Arc<dyn ProgressSink> =
            Arc::new(ThrottledProgress::new(progress, PROGRESS_INTERVAL));
        match self
            .run_upload(&id, &new, size, content_type, created, &progress, &cancel)
            .await
        {
            Ok(item) => {
                progress.report(size, Some(size));
                Ok(item)
            }
            Err(e) => {
                self.abort(&id).await;
                Err(e)
            }
        }
    }

    /// `GET /api/items`: one page of items, newest first.
    pub async fn list(&self, query: &ListQuery) -> Result<Page, CloudError> {
        let mut pairs: Vec<(&str, String)> = Vec::new();
        if let Some(l) = query.limit {
            pairs.push(("limit", l.to_string()));
        }
        if let Some(c) = &query.cursor {
            pairs.push(("cursor", c.clone()));
        }
        if let Some(q) = &query.q {
            pairs.push(("q", q.clone()));
        }
        if let Some(k) = query.kind {
            pairs.push(("kind", k.as_str().to_owned()));
        }
        if query.favorite {
            pairs.push(("favorite", "1".to_owned()));
        }
        let mut url = self.endpoint("api/items")?;
        if !pairs.is_empty() {
            url.query_pairs_mut()
                .extend_pairs(pairs.iter().map(|(k, v)| (*k, v.as_str())));
        }
        let bytes = self.send_api(Method::GET, url, None, None).await?;
        parse_json(&bytes)
    }

    /// `POST /api/items/:id/shares`: create a public link. `None` means it never expires.
    /// The returned [`Share::url`] is the only time the server reveals the link.
    pub async fn create_share(
        &self,
        item_id: &str,
        expires_in: Option<Duration>,
    ) -> Result<Share, CloudError> {
        let body = CreateShareRequest {
            expires_in_seconds: expires_in.map(|d| d.as_secs()),
        };
        let path = format!("api/items/{}/shares", seg(item_id));
        let r: CreateShareResponse = self.json(Method::POST, &path, Some(&body), None).await?;
        let mut share = r.share;
        share.url = Some(r.url);
        Ok(share)
    }

    /// `DELETE /api/shares/:id`: the link stops working immediately.
    pub async fn revoke_share(&self, share_id: &str) -> Result<(), CloudError> {
        let url = self.endpoint(&format!("api/shares/{}", seg(share_id)))?;
        self.send_api(Method::DELETE, url, None, None)
            .await
            .map(drop)
    }

    /// `DELETE /api/items/:id`: removes the media, thumbnail and its shares.
    pub async fn delete_item(&self, item_id: &str) -> Result<(), CloudError> {
        let url = self.endpoint(&format!("api/items/{}", seg(item_id)))?;
        self.send_api(Method::DELETE, url, None, None)
            .await
            .map(drop)
    }
}

/// Knobs for tests and local development. Not part of the normal surface.
impl CloudClient {
    /// Send `X-Dev-Identity: identity` with every API call. The server honours it only in
    /// `dev`/`test` mode on `localhost`; it is never sent to storage URLs.
    #[doc(hidden)]
    #[must_use]
    pub fn with_dev_identity(mut self, identity: &str) -> Self {
        if let Ok(v) = HeaderValue::try_from(identity) {
            self.extra_headers
                .push((HeaderName::from_static("x-dev-identity"), v));
        }
        self
    }

    /// Base delay of the part retry backoff (doubles per retry, capped at 8 s).
    #[doc(hidden)]
    #[must_use]
    pub fn with_retry_backoff(mut self, base: Duration) -> Self {
        self.backoff = base;
        self
    }

    /// Ignore `HTTP(S)_PROXY` (loopback test servers).
    #[doc(hidden)]
    pub fn without_proxy(mut self) -> Result<Self, CloudError> {
        (self.api, self.storage) = build_clients(true)?;
        Ok(self)
    }
}

// ---- upload state machine ----------------------------------------------------------------

enum PartFail {
    /// Not worth retrying.
    Fatal(CloudError),
    /// The presigned URL was rejected (403): fetch a new one.
    Expired(CloudError),
    /// Transient: back off and try again.
    Retry(CloudError),
}

struct PartUrls {
    urls: std::collections::HashMap<u32, String>,
}

impl CloudClient {
    #[allow(clippy::too_many_arguments, reason = "internal plumbing of one upload")]
    async fn run_upload(
        &self,
        id: &str,
        new: &NewUpload,
        size: u64,
        content_type: &str,
        created: CreateUploadResponse,
        progress: &Arc<dyn ProgressSink>,
        cancel: &CancellationToken,
    ) -> Result<Item, CloudError> {
        let parts = match created.upload {
            UploadTarget::Single { url, headers } => {
                self.put_single(
                    &url,
                    &headers,
                    content_type,
                    &new.path,
                    size,
                    progress,
                    cancel,
                )
                .await?;
                None
            }
            UploadTarget::Multipart {
                part_size,
                part_count,
            } => Some(
                self.put_parts(id, &new.path, size, part_size, part_count, progress, cancel)
                    .await?,
            ),
        };
        ensure_unchanged(&new.path, size).await?;
        if let (Some(jpeg), Some(UploadTarget::Single { url, headers })) =
            (&new.thumbnail_jpeg, &created.thumbnail_upload)
            && let Err(e) = self.put_thumbnail(url, headers, jpeg.clone(), cancel).await
        {
            if e.is_cancelled() {
                return Err(e);
            }
            tracing::warn!(error = %e, "thumbnail upload failed; continuing without it");
        }
        check_cancel(cancel)?;
        let body = CompleteRequest { parts };
        let path = format!("api/uploads/{}/complete", seg(id));
        // `complete` is idempotent server-side, so a dropped response may be retried.
        let mut attempt = 0;
        let envelope: ItemEnvelope = loop {
            match self
                .json(Method::POST, &path, Some(&body), Some(cancel))
                .await
            {
                Ok(v) => break v,
                Err(e @ (CloudError::Network { .. } | CloudError::Http { status: 500.., .. }))
                    if attempt < 2 =>
                {
                    tracing::warn!(error = %e, "completing the upload failed; retrying");
                    attempt += 1;
                    self.sleep(self.backoff_for(attempt), cancel).await?;
                }
                Err(e) => return Err(e),
            }
        };
        Ok(envelope.item)
    }

    #[allow(clippy::too_many_arguments, reason = "internal plumbing of one upload")]
    async fn put_single(
        &self,
        url: &str,
        headers: &std::collections::HashMap<String, String>,
        content_type: &str,
        path: &Path,
        size: u64,
        progress: &Arc<dyn ProgressSink>,
        cancel: &CancellationToken,
    ) -> Result<(), CloudError> {
        let ctx = UploadContext::new(self.storage.clone())
            .with_progress(progress.clone())
            .with_cancel(cancel.clone());
        let plan = BodyPlan::raw(Payload::File {
            path: path.to_owned(),
            len: size,
        });
        let (body, fault) = plan.into_body(&ctx).await.map_err(map_upload_error)?;
        let mut req = self
            .storage
            .put(parse_storage_url(url)?)
            .header(CONTENT_LENGTH, size);
        let mut has_type = false;
        for (k, v) in headers {
            has_type |= k.eq_ignore_ascii_case("content-type");
            req = req.header(header_name(k)?, header_value(v)?);
        }
        if !has_type {
            req = req.header(CONTENT_TYPE, content_type);
        }
        let sent = tokio::select! {
            () = cancel.cancelled() => return Err(CloudError::Cancelled),
            r = req.body(body).send() => r,
        };
        let resp = match sent {
            Ok(r) => r,
            // Surface the real local cause (file grew/shrank, read error) if there was one.
            Err(e) => {
                return Err(match upload::http::map_send_error(&ctx, &e, Some(&fault)) {
                    UploadError::Cancelled => CloudError::Cancelled,
                    UploadError::Io { .. } => CloudError::SourceChanged,
                    _ => net_error(e),
                });
            }
        };
        storage_status(&resp)
    }

    async fn put_thumbnail(
        &self,
        url: &str,
        headers: &std::collections::HashMap<String, String>,
        jpeg: Bytes,
        cancel: &CancellationToken,
    ) -> Result<(), CloudError> {
        let mut req = self
            .storage
            .put(parse_storage_url(url)?)
            .header(CONTENT_TYPE, "image/jpeg")
            .header(CONTENT_LENGTH, jpeg.len());
        for (k, v) in headers {
            req = req.header(header_name(k)?, header_value(v)?);
        }
        let resp = race(Some(cancel), req.body(jpeg).send()).await?;
        storage_status(&resp)
    }

    #[allow(clippy::too_many_arguments, reason = "internal plumbing of one upload")]
    async fn put_parts(
        &self,
        id: &str,
        path: &Path,
        size: u64,
        part_size: u64,
        part_count: u32,
        progress: &Arc<dyn ProgressSink>,
        cancel: &CancellationToken,
    ) -> Result<Vec<CompletedPart>, CloudError> {
        if part_size == 0
            || part_count == 0
            || part_count > MAX_PARTS
            || size.div_ceil(part_size) != u64::from(part_count)
        {
            return Err(CloudError::invalid(format!(
                "inconsistent multipart plan: {size} bytes, {part_count} parts of {part_size}"
            )));
        }
        let changed = Arc::new(AtomicBool::new(false));
        let mut urls = PartUrls {
            urls: std::collections::HashMap::new(),
        };
        let mut done = Vec::with_capacity(part_count as usize);
        for n in 1..=part_count {
            check_cancel(cancel)?;
            let offset = u64::from(n - 1) * part_size;
            let len = part_size.min(size - offset);
            let mut attempt = 0;
            let mut refresh = false;
            let etag = loop {
                if refresh || !urls.urls.contains_key(&n) {
                    self.fetch_part_urls(id, n, part_count, &mut urls, cancel)
                        .await?;
                    refresh = false;
                }
                let url = urls.urls.get(&n).cloned().ok_or_else(|| {
                    CloudError::invalid(format!("server returned no URL for part {n}"))
                })?;
                let sink = PartSink {
                    progress: progress.clone(),
                    cancel: cancel.clone(),
                    base: offset,
                    total: size,
                    changed: changed.clone(),
                };
                let outcome = self
                    .try_part(&url, path, offset, len, sink, cancel, &changed)
                    .await;
                let err = match outcome {
                    Ok(etag) => break etag,
                    Err(PartFail::Fatal(e)) => return Err(e),
                    Err(PartFail::Expired(e)) => {
                        refresh = true;
                        e
                    }
                    Err(PartFail::Retry(e)) => e,
                };
                if attempt >= PART_RETRIES {
                    return Err(err);
                }
                attempt += 1;
                tracing::warn!(part = n, attempt, error = %err, "part upload failed; retrying");
                if !refresh {
                    self.sleep(self.backoff_for(attempt), cancel).await?;
                }
            };
            done.push(CompletedPart {
                part_number: n,
                etag,
            });
        }
        Ok(done)
    }

    /// Ask for URLs for parts `from..from+8` (clamped to `count`).
    async fn fetch_part_urls(
        &self,
        id: &str,
        from: u32,
        count: u32,
        urls: &mut PartUrls,
        cancel: &CancellationToken,
    ) -> Result<(), CloudError> {
        let last = count.min(from + PART_URL_BATCH - 1);
        let body = PartsRequest {
            part_numbers: (from..=last).collect(),
        };
        let path = format!("api/uploads/{}/parts", seg(id));
        let r: PartsResponse = self
            .json(Method::POST, &path, Some(&body), Some(cancel))
            .await?;
        for p in r.parts {
            urls.urls.insert(p.part_number, p.url);
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments, reason = "internal plumbing of one upload")]
    async fn try_part(
        &self,
        url: &str,
        path: &Path,
        offset: u64,
        len: u64,
        sink: PartSink,
        cancel: &CancellationToken,
        changed: &AtomicBool,
    ) -> Result<String, PartFail> {
        let url = parse_storage_url(url).map_err(PartFail::Fatal)?;
        let body = ranged_body(path, offset, len, sink)
            .await
            .map_err(PartFail::Fatal)?;
        let sent = race(
            Some(cancel),
            self.storage
                .put(url)
                .header(CONTENT_LENGTH, len)
                .body(body)
                .send(),
        )
        .await;
        let resp = match sent {
            Ok(r) => r,
            Err(e @ CloudError::Network { .. }) => {
                return Err(if changed.load(Ordering::SeqCst) {
                    PartFail::Fatal(CloudError::SourceChanged)
                } else {
                    PartFail::Retry(e)
                });
            }
            Err(e) => return Err(PartFail::Fatal(e)),
        };
        let status = resp.status();
        if status.is_success() {
            return resp
                .headers()
                .get(ETAG)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned)
                .ok_or_else(|| PartFail::Fatal(CloudError::invalid("part response had no ETag")));
        }
        let err = CloudError::Http {
            status: status.as_u16(),
            code: None,
            message: "part upload rejected".to_owned(),
        };
        Err(match status.as_u16() {
            403 => PartFail::Expired(err),
            408 | 429 | 500..=599 => PartFail::Retry(err),
            _ => PartFail::Fatal(err),
        })
    }

    fn backoff_for(&self, attempt: u32) -> Duration {
        self.backoff
            .saturating_mul(1 << attempt.saturating_sub(1).min(4))
            .min(Duration::from_secs(8))
    }

    async fn sleep(&self, d: Duration, cancel: &CancellationToken) -> Result<(), CloudError> {
        tokio::select! {
            () = cancel.cancelled() => Err(CloudError::Cancelled),
            () = tokio::time::sleep(d) => Ok(()),
        }
    }

    /// Best-effort `DELETE /api/uploads/:id`, bounded and deliberately not cancellable.
    async fn abort(&self, id: &str) {
        let Ok(url) = self.endpoint(&format!("api/uploads/{}", seg(id))) else {
            return;
        };
        let call = self.send_api(Method::DELETE, url, None, None);
        match tokio::time::timeout(ABORT_TIMEOUT, call).await {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => tracing::warn!(error = %e, "could not abort the upload on the server"),
            Err(_) => tracing::warn!("aborting the upload on the server timed out"),
        }
    }
}

// ---- API plumbing ------------------------------------------------------------------------

impl CloudClient {
    fn endpoint(&self, path: &str) -> Result<Url, CloudError> {
        self.base
            .join(path)
            .map_err(|e| CloudError::config(format!("bad API path: {e}")))
    }

    async fn json<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<&impl Serialize>,
        cancel: Option<&CancellationToken>,
    ) -> Result<T, CloudError> {
        let body = body
            .map(serde_json::to_vec)
            .transpose()
            .map_err(|e| CloudError::invalid(format!("could not encode request: {e}")))?;
        let bytes = self
            .send_api(method, self.endpoint(path)?, body, cancel)
            .await?;
        parse_json(&bytes)
    }

    /// One API call with credentials attached. Only ever called with URLs built from
    /// `self.base`, so the service token cannot reach another origin.
    async fn send_api(
        &self,
        method: Method,
        url: Url,
        body: Option<Vec<u8>>,
        cancel: Option<&CancellationToken>,
    ) -> Result<Bytes, CloudError> {
        debug_assert_eq!(url.origin(), self.base.origin());
        let mut req = self.api.request(method, url);
        let mut id = HeaderValue::try_from(self.client_id.as_str())
            .map_err(|_| CloudError::config("the client id contains invalid characters"))?;
        let mut secret = HeaderValue::try_from(self.client_secret.as_str())
            .map_err(|_| CloudError::config("the client secret contains invalid characters"))?;
        id.set_sensitive(true);
        secret.set_sensitive(true);
        req = req
            .header("cf-access-client-id", id)
            .header("cf-access-client-secret", secret);
        for (k, v) in &self.extra_headers {
            req = req.header(k, v);
        }
        if let Some(b) = body {
            req = req.header(CONTENT_TYPE, "application/json").body(b);
        }
        let mut resp = race(cancel, req.send()).await?;
        let status = resp.status();
        if status.is_redirection() {
            return Err(CloudError::Auth {
                message: "Cloudflare Access redirected to its login page: the service token was \
                          rejected. Check the client id and secret, and that the token is allowed \
                          by the Access policy."
                    .to_owned(),
            });
        }
        let mut buf = BytesMut::new();
        loop {
            let chunk = race(cancel, resp.chunk()).await?;
            let Some(chunk) = chunk else { break };
            if buf.len() + chunk.len() > MAX_API_BODY {
                return Err(CloudError::invalid("response body too large"));
            }
            buf.extend_from_slice(&chunk);
        }
        let buf = buf.freeze();
        if status.is_success() {
            Ok(buf)
        } else {
            Err(api_error(status, &buf))
        }
    }
}

fn api_error(status: StatusCode, body: &[u8]) -> CloudError {
    let envelope: Option<serde_json::Value> = serde_json::from_slice(body).ok();
    let field = |k: &str| {
        envelope
            .as_ref()
            .and_then(|v| v.get("error"))
            .and_then(|e| e.get(k))
            .and_then(|s| s.as_str())
            .map(str::to_owned)
    };
    let code = field("code");
    let message = field("message").unwrap_or_else(|| snippet(body));
    if matches!(status.as_u16(), 401 | 403) {
        return CloudError::Auth {
            message: format!(
                "the server rejected the credentials ({status}); check the service token{}",
                if message.is_empty() {
                    String::new()
                } else {
                    format!(": {message}")
                }
            ),
        };
    }
    CloudError::Http {
        status: status.as_u16(),
        code,
        message,
    }
}

fn snippet(body: &[u8]) -> String {
    String::from_utf8_lossy(body).chars().take(200).collect()
}

fn parse_json<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, CloudError> {
    serde_json::from_slice(bytes).map_err(|e| {
        CloudError::invalid(format!(
            "could not parse the response ({e}): {}",
            snippet(bytes)
        ))
    })
}

/// Await `fut`, abandoning it (and so the connection) when `cancel` fires.
async fn race<T>(
    cancel: Option<&CancellationToken>,
    fut: impl std::future::Future<Output = Result<T, reqwest::Error>>,
) -> Result<T, CloudError> {
    let out = match cancel {
        Some(c) => tokio::select! {
            () = c.cancelled() => return Err(CloudError::Cancelled),
            r = fut => r,
        },
        None => fut.await,
    };
    out.map_err(net_error)
}

fn net_error(e: reqwest::Error) -> CloudError {
    let e = e.without_url();
    let mut message = String::new();
    let mut cur: Option<&(dyn std::error::Error + 'static)> = Some(&e);
    while let Some(err) = cur {
        let part = err.to_string();
        if !part.is_empty() && !message.contains(&part) {
            if !message.is_empty() {
                message.push_str(": ");
            }
            message.push_str(&part);
        }
        cur = err.source();
    }
    CloudError::Network { message }
}

fn check_cancel(cancel: &CancellationToken) -> Result<(), CloudError> {
    if cancel.is_cancelled() {
        Err(CloudError::Cancelled)
    } else {
        Ok(())
    }
}

fn storage_status(resp: &reqwest::Response) -> Result<(), CloudError> {
    let status = resp.status();
    if status.is_success() {
        Ok(())
    } else {
        Err(CloudError::Http {
            status: status.as_u16(),
            code: None,
            message: "storage rejected the upload".to_owned(),
        })
    }
}

async fn ensure_unchanged(path: &Path, size: u64) -> Result<(), CloudError> {
    let now = tokio::fs::metadata(path)
        .await
        .map_err(|e| CloudError::io(format!("reading {}", path.display()), e))?;
    if now.len() == size {
        Ok(())
    } else {
        Err(CloudError::SourceChanged)
    }
}

fn map_upload_error(e: UploadError) -> CloudError {
    match e {
        UploadError::Cancelled => CloudError::Cancelled,
        UploadError::Io { context, source } => CloudError::Io { context, source },
        other => CloudError::config(other.to_string()),
    }
}

fn parse_storage_url(url: &str) -> Result<Url, CloudError> {
    let u =
        Url::parse(url).map_err(|_| CloudError::invalid("server sent an invalid upload URL"))?;
    if matches!(u.scheme(), "http" | "https") {
        Ok(u)
    } else {
        Err(CloudError::invalid(
            "server sent an upload URL with an unsupported scheme",
        ))
    }
}

fn header_name(k: &str) -> Result<HeaderName, CloudError> {
    HeaderName::from_bytes(k.as_bytes())
        .map_err(|_| CloudError::invalid("server sent an invalid upload header name"))
}

fn header_value(v: &str) -> Result<HeaderValue, CloudError> {
    HeaderValue::try_from(v)
        .map_err(|_| CloudError::invalid("server sent an invalid upload header value"))
}

/// Percent-encode one path segment.
fn seg(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

/// Content type from the file extension, restricted to what the server allows. Unknown
/// extensions get `application/octet-stream`, which the server rejects as unsupported.
fn content_type_for(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    match ext.as_deref() {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        Some("gif") => "image/gif",
        Some("mp4" | "m4v") => "video/mp4",
        Some("webm") => "video/webm",
        Some("mkv") => "video/x-matroska",
        Some("mov") => "video/quicktime",
        _ => "application/octet-stream",
    }
}
