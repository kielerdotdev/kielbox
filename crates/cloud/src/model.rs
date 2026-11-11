//! Wire types of the Kielbox Cloud JSON API (`plans/cloud.md` section 5).

use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;

use bytes::Bytes;
use serde::{Deserialize, Serialize};
use url::Url;

/// What kind of capture an item is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ItemKind {
    /// A still image.
    Screenshot,
    /// A screen recording.
    Recording,
    /// An instant-replay save.
    Replay,
    /// A trimmed clip.
    Clip,
    /// Anything else.
    #[default]
    #[serde(other)]
    Other,
}

impl ItemKind {
    /// The wire / query-string spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Screenshot => "screenshot",
            Self::Recording => "recording",
            Self::Replay => "replay",
            Self::Clip => "clip",
            Self::Other => "other",
        }
    }
}

/// Connection settings. The secret is redacted from `Debug`.
#[derive(Clone)]
pub struct CloudConfig {
    /// Root of the app hostname, e.g. `https://cloud.example.com`. Must be `https`
    /// (`http` only for `localhost` / `127.0.0.1`).
    pub base_url: Url,
    /// Access service token id (`CF-Access-Client-Id`).
    pub client_id: String,
    /// Access service token secret (`CF-Access-Client-Secret`).
    pub client_secret: String,
}

impl fmt::Debug for CloudConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CloudConfig")
            .field("base_url", &self.base_url.as_str())
            .field("client_id", &self.client_id)
            .field("client_secret", &"<redacted>")
            .finish()
    }
}

/// One file to upload.
#[derive(Debug, Clone)]
pub struct NewUpload {
    /// File to send. Its extension decides the content type.
    pub path: PathBuf,
    /// Library category.
    pub kind: ItemKind,
    /// Display title; the server defaults it to the file name stem.
    pub title: Option<String>,
    /// When the capture was made, unix milliseconds.
    pub captured_at_ms: i64,
    /// Pixel width, if known.
    pub width: Option<u32>,
    /// Pixel height, if known.
    pub height: Option<u32>,
    /// Duration in milliseconds, if it is a video.
    pub duration_ms: Option<u64>,
    /// JPEG thumbnail (the server accepts up to 1 MiB).
    pub thumbnail_jpeg: Option<Bytes>,
}

/// Whether the caller is a person or a service token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IdentityKind {
    /// An Access user.
    User,
    /// An Access service token.
    Service,
}

/// Server limits reported by `GET /api/me`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub struct Limits {
    /// Largest accepted upload in bytes.
    pub max_upload_bytes: u64,
    /// Files at or above this size use multipart.
    pub multipart_threshold: u64,
    /// Default multipart part size.
    pub part_size: u64,
}

/// Response of `GET /api/me`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Me {
    /// Email or service-token name.
    pub identity: String,
    /// User or service token.
    pub kind: IdentityKind,
    /// Server limits.
    pub limits: Limits,
}

/// A library item. Object keys never appear here.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(default)]
pub struct Item {
    /// Item id (ULID).
    pub id: String,
    /// `pending` or `ready`.
    pub status: String,
    /// Category.
    pub kind: ItemKind,
    /// Display title.
    pub title: String,
    /// Original file name.
    pub filename: String,
    /// MIME type.
    pub content_type: String,
    /// Size in bytes.
    pub size: u64,
    /// Pixel width.
    pub width: Option<u32>,
    /// Pixel height.
    pub height: Option<u32>,
    /// Duration in milliseconds.
    pub duration_ms: Option<u64>,
    /// Capture time, unix milliseconds.
    pub captured_at: i64,
    /// Upload time, unix milliseconds.
    pub created_at: i64,
    /// Uploader identity.
    pub created_by: String,
    /// Favourite flag.
    pub favorite: bool,
    /// Relative API path of the media.
    pub media_url: String,
    /// Relative API path of the thumbnail, when there is one.
    pub thumb_url: Option<String>,
}

/// Filters for [`crate::CloudClient::list`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ListQuery {
    /// Page size, 1..=100 (server default when `None`).
    pub limit: Option<u32>,
    /// `next_cursor` of the previous page.
    pub cursor: Option<String>,
    /// Substring search over title and file name.
    pub q: Option<String>,
    /// Restrict to one kind.
    pub kind: Option<ItemKind>,
    /// Only favourites.
    pub favorite: bool,
}

/// One page of items, newest first.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Page {
    /// The items.
    pub items: Vec<Item>,
    /// Pass as [`ListQuery::cursor`] for the next page; `None` at the end.
    pub next_cursor: Option<String>,
}

/// A share link record.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(default)]
pub struct Share {
    /// Share id (not the token).
    pub id: String,
    /// The shared item.
    pub item_id: String,
    /// Creation time, unix milliseconds.
    pub created_at: i64,
    /// Expiry, unix milliseconds; `None` = never.
    pub expires_at: Option<i64>,
    /// Revocation time, if revoked.
    pub revoked_at: Option<i64>,
    /// Whether viewers may download.
    pub allow_download: bool,
    /// Page views so far.
    pub view_count: u64,
    /// The public link. Present only on the value returned by
    /// [`crate::CloudClient::create_share`]; the server never shows it again.
    #[serde(skip)]
    pub url: Option<String>,
}

// ---- internal request / response shapes -------------------------------------------------

#[derive(Debug, Serialize)]
pub(crate) struct CreateUploadRequest<'a> {
    pub filename: &'a str,
    pub size: u64,
    pub content_type: &'a str,
    pub kind: ItemKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<&'a str>,
    pub captured_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    pub thumbnail: bool,
}

#[derive(Debug, Deserialize)]
pub(crate) struct CreateUploadResponse {
    pub item_id: String,
    pub upload: UploadTarget,
    pub thumbnail_upload: Option<UploadTarget>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "mode", rename_all = "lowercase")]
pub(crate) enum UploadTarget {
    Single {
        url: String,
        #[serde(default)]
        headers: HashMap<String, String>,
    },
    Multipart {
        part_size: u64,
        part_count: u32,
    },
}

#[derive(Debug, Serialize)]
pub(crate) struct PartsRequest {
    pub part_numbers: Vec<u32>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct PartsResponse {
    pub parts: Vec<PartUrl>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct PartUrl {
    pub part_number: u32,
    pub url: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct CompletedPart {
    pub part_number: u32,
    pub etag: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct CompleteRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parts: Option<Vec<CompletedPart>>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ItemEnvelope {
    pub item: Item,
}

#[derive(Debug, Serialize)]
pub(crate) struct CreateShareRequest {
    pub expires_in_seconds: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct CreateShareResponse {
    pub share: Share,
    pub url: String,
}
