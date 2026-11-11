//! Client for Kielbox Cloud, the personal Cloudflare-hosted library (`plans/cloud.md`).
//!
//! * [`CloudClient`] talks to the JSON API behind Cloudflare Access using a service token
//!   (`CF-Access-Client-Id` / `CF-Access-Client-Secret`). The token is sent to the API
//!   origin only, never to the presigned R2 URLs the bytes go to, and redirects are not
//!   followed: an Access login redirect means the token was rejected and becomes
//!   [`CloudError::Auth`].
//! * [`CloudClient::upload`] streams a file straight to R2, as one presigned `PUT` or as
//!   sequential presigned multipart parts, built on the `upload` crate's HTTP client,
//!   progress and cancellation conventions.
//! * [`CloudClient::list`], [`create_share`](CloudClient::create_share),
//!   [`revoke_share`](CloudClient::revoke_share) and
//!   [`delete_item`](CloudClient::delete_item) cover the rest of what the desktop needs.
#![forbid(unsafe_code)]

mod body;
mod client;
mod error;
mod model;

pub use client::CloudClient;
pub use error::CloudError;
pub use model::{
    CloudConfig, IdentityKind, Item, ItemKind, Limits, ListQuery, Me, NewUpload, Page, Share,
};
