# Kielbox Cloud — implementation plan

Status: milestones M1-M6 implemented and tested on Linux; M7 documentation written. Not yet
validated: no browser run of the explorer (M4 checklist), no Windows run or Wine render of the
desktop (M6), no real deployment (M7 smoke test). See "Implementation notes" at the end. Written
2026-11-11 against `29cf8c2`.

A personal, single-tenant cloud for Kielbox captures: a TanStack Start application on
Cloudflare Workers that hands out presigned R2 upload URLs, keeps a metadata index in D1,
serves a web library explorer behind Cloudflare Access, and issues revocable share links that
work without an Access login. The desktop app uploads captures to it and copies share links.

Read this whole file before starting. Implement the milestones in order; each one ends in a
state that builds, passes its tests and can be committed on its own.

## 1. Decisions already made

Do not reopen these while implementing. Section 13 lists the ones the owner may still change.

| Topic | Decision | Why |
|---|---|---|
| Tenancy | Single tenant. Everyone who passes the Access policy sees one shared library. `created_by` is recorded for audit only. | "Basic cloud"; Access already decides who is in. |
| Upload path | Bytes go desktop → R2 directly through presigned S3 URLs. The Worker never proxies upload bodies. | Workers cap request bodies at 100 MB on Free/Pro; recordings are multi-GB. |
| Large files | Single presigned `PUT` below 64 MiB, presigned multipart at or above it. | R2 single PUT tops out near 5 GiB; multipart also gives per-part retry. |
| Download path | The Worker streams from the R2 **binding** with `Range` support. No presigned GET, no public bucket, no `r2.dev` URL. | Revoking a share takes effect immediately; the S3 endpoint and keys never reach viewers; R2 egress is free. |
| Metadata | D1 (SQLite). | Listing, search, paging and share lookups need an index; R2 `list` is not enough. |
| Auth | Cloudflare Access in front of the app hostname **and** the Worker verifies `Cf-Access-Jwt-Assertion` itself on every request to the app hostname (pages, server functions, API). Fails closed. | A missing or mis-scoped Access application must not expose the library. |
| Desktop auth | Access **service token** (`CF-Access-Client-Id` / `CF-Access-Client-Secret` headers), stored in the OS keyring. | No embedded browser or `cloudflared` dependency. |
| Shares | Two hostnames on one Worker: `cloud.<domain>` (Access) and `share.<domain>` (public, serves only `/s/*`). | Shared media is user-supplied content. Serving it from the Access origin would let a hostile file reach `/api` with the viewer's `CF_Authorization` cookie. |
| Share tokens | 32 random bytes, base64url. Only the SHA-256 is stored. Optional expiry, revocable, per item. | A D1 leak must not yield working links. |
| Web stack | One TanStack Start (React) app in `cloud/`, deployed to Workers with `@cloudflare/vite-plugin`. UI is Tailwind v4 + Base UI primitives, themed with the desktop's Quiet tokens. Besides those and Start, Router and Query: `hono` (desktop JSON API), `zod` (input validation), `aws4fetch` (SigV4 presign), `jose` (Access JWT). | Owner's choice of framework; the other four are small and run on Workers unmodified. |
| Who calls what | The explorer uses `createServerFn` server functions. The desktop uses a plain JSON API under `/api/*`. Both call the same framework-free services in `src/server/`. | The desktop contract must not depend on Start's internal RPC format, and the core must be testable without building the app. |
| Share page | Plain server-rendered HTML with no JavaScript, produced by the Worker entry — not a Start route. | Allows a no-script CSP on the only public surface and keeps the app bundle off the public hostname. |
| Rust client | New crate `crates/cloud`, built on `upload`'s HTTP client, streaming body, progress and cancellation. | Those are already tested for multi-GB streaming (`crates/upload/tests/streaming_memory.rs`). |

Confirm every Cloudflare limit quoted here against the current docs before hard-coding it
(R2 multipart limits, Workers body limit, presigned URL maximum lifetime, Access header names).

## 2. Repository layout

```
cloud/                         new; NOT a Cargo workspace member (members = crates/*)
  package.json                 scripts: dev (vite dev), build, test, typecheck, deploy, migrate
  vite.config.ts               cloudflare vite plugin + tanstackStart + react
  wrangler.jsonc
  tsconfig.json
  vitest.config.ts             @cloudflare/vitest-pool-workers; covers src/server/** only
  migrations/0001_init.sql
  src/
    server.ts                  Worker entry: hostname dispatch, auth gate, scheduled()
    router.tsx                 router + QueryClient
    routes/
      __root.tsx               document shell, styles, error/not-found
      index.tsx                library grid
      items.$itemId.tsx        detail, player, shares panel
    functions/                 createServerFn wrappers (explorer only)
      auth.ts                  authMiddleware + authedFn factory
      items.ts  shares.ts  me.ts
    components/                Card, Grid, FilterChips, Player, SharePanel
    components/ui/             styled wrappers around Base UI primitives
    styles.css                 Tailwind entry + Quiet tokens in @theme
    server/                    framework-free; imports nothing from TanStack or React
      env.ts                   Env type + validated config (throws on missing vars)
      auth/access.ts           Access JWT verification
      auth/origin.ts           Origin check for browser mutations
      storage/signer.ts        UploadSigner interface
      storage/r2_s3.ts         production signer (aws4fetch against the R2 S3 API)
      storage/local.ts         dev/test signer (HMAC URLs handled by the Worker itself)
      db/items.ts  db/shares.ts            all SQL lives here, prepared statements only
      services/uploads.ts  items.ts  shares.ts   business rules; the only callers of db + storage
      api.ts                   hono app for /api/* (the desktop's contract, section 5.1)
      public.ts                share-host handler (section 5.2)
      share_page.ts            plain server-rendered share HTML, no JavaScript
      media.ts                 Range streaming + response headers
      cleanup.ts               cron body
      ids.ts                   item ids, share tokens, hashing
  test/                        *.test.ts
crates/cloud/                  new Rust client crate (GPL-3.0-or-later, edition 2024)
crates/desktop/src/cloud.rs        worker thread + local sync state
crates/desktop/src/rune_cloud.rs   Settings tab and card controls
docs/CLOUD.md                  setup and operations guide
```

`src/server/services/` has three thin callers and no logic is duplicated between them: the
hono API (desktop), the server functions (explorer) and the share-host handler (public).
Tests target `src/server/**` directly in the Workers Vitest pool, so they need neither a Start
build nor a browser.

Add `/cloud/node_modules/`, `/cloud/.wrangler/`, `/cloud/dist/`, `/cloud/.tanstack/` and
`/cloud/.dev.vars` to `.gitignore`. Commit `cloud/package-lock.json` and
`src/routeTree.gen.ts`, and pin exact versions, matching how Cargo deps are pinned.

Scaffold with `npm create cloudflare@latest -- --framework=tanstack-start` and adapt the
result to this layout rather than hand-writing the Vite, wrangler and entry files: TanStack
Start's Workers integration has changed between releases, so the generated project and the
current "Hosting → Cloudflare Workers" page of the Start docs are authoritative over the
snippets here. What must hold whatever the exact API: a custom Worker entry that can refuse
a request before Start sees it, and a `scheduled` export.

```ts
// src/server.ts — shape only
import startHandler from "@tanstack/react-start/server-entry";
export default {
  async fetch(request: Request, env: Env, ctx: ExecutionContext) {
    /* section 5.3 */ return startHandler.fetch(request);
  },
  async scheduled(_: ScheduledController, env: Env, ctx: ExecutionContext) { /* section 7 */ },
};
```

## 3. Cloudflare resources and configuration

`wrangler.jsonc` (names are placeholders; real ids go in the owner's deployment, not in git —
keep `database_id` and routes in an untracked `wrangler.local.jsonc` or environment section
documented in `docs/CLOUD.md`):

- R2 bucket binding `BUCKET` → `kielbox-cloud`.
- D1 binding `DB` → `kielbox-cloud`, `migrations_dir: "migrations"`.
- `main: "./src/server.ts"`, `compatibility_flags: ["nodejs_compat"]`.
- Assets binding `ASSETS` (the Vite plugin supplies the client build directory) with
  `run_worker_first: true`, so the entry decides per hostname whether a file may be served.
  If the plugin makes worker-first assets impractical, serving the client bundle ungated is
  an acceptable fallback: it contains code only, and the app hostname is behind Access at the
  edge regardless. The share-host and `/api` rules do not depend on it.
- Cron trigger `17 3 * * *` for cleanup.
- Routes: custom domains `cloud.<domain>` and `share.<domain>`. Set `workers_dev: false` and
  `preview_urls: false` — a `*.workers.dev` URL would sit outside Access.
- Vars: `APP_HOST`, `SHARE_HOST`, `ACCESS_TEAM_DOMAIN` (`https://<team>.cloudflareaccess.com`),
  `ACCESS_AUD`, `R2_ACCOUNT_ID`, `R2_BUCKET_NAME`, `MAX_UPLOAD_BYTES` (default 20 GiB),
  `ENVIRONMENT` (`production` | `dev` | `test`).
- Secrets (`wrangler secret put`): `R2_ACCESS_KEY_ID`, `R2_SECRET_ACCESS_KEY` — an R2 API
  token scoped to **Object Read & Write on this one bucket**. `LOCAL_SIGNING_KEY` exists only
  in `.dev.vars`.

Access (Zero Trust dashboard; document step by step in `docs/CLOUD.md`):

1. Self-hosted application for `cloud.<domain>` (all paths). Policy 1: Allow, the owner's
   identity. Policy 2: **Service Auth**, the service token `kielbox-desktop`.
2. No Access application on `share.<domain>`.
3. Copy the application's AUD tag into `ACCESS_AUD`.

R2 bucket: add a lifecycle rule aborting incomplete multipart uploads after 1 day. No CORS
rule is needed (only the desktop uploads; browsers never talk to the S3 endpoint).

## 4. Data model

`migrations/0001_init.sql`:

```sql
CREATE TABLE items (
  id            TEXT PRIMARY KEY,            -- 26-char ULID, generated by the Worker
  status        TEXT NOT NULL CHECK (status IN ('pending','ready')),
  kind          TEXT NOT NULL CHECK (kind IN ('screenshot','recording','replay','clip','other')),
  title         TEXT NOT NULL,
  filename      TEXT NOT NULL,               -- original name, display/download only
  content_type  TEXT NOT NULL,
  size          INTEGER NOT NULL,
  object_key    TEXT NOT NULL UNIQUE,        -- u/<id>/original.<ext>
  thumb_key     TEXT,                        -- u/<id>/thumb.jpg, set once uploaded
  width         INTEGER, height INTEGER, duration_ms INTEGER,
  captured_at   INTEGER NOT NULL,            -- unix ms, from the client
  created_at    INTEGER NOT NULL,            -- unix ms, server clock
  created_by    TEXT NOT NULL,               -- Access email or service-token common_name
  favorite      INTEGER NOT NULL DEFAULT 0,
  upload_id     TEXT,                        -- multipart only, cleared on completion
  part_size     INTEGER
);
CREATE INDEX items_ready_by_date ON items (status, captured_at DESC, id DESC);
CREATE INDEX items_pending_age   ON items (status, created_at);

CREATE TABLE shares (
  id             TEXT PRIMARY KEY,           -- ULID, safe to show in the UI
  item_id        TEXT NOT NULL REFERENCES items(id) ON DELETE CASCADE,
  token_hash     TEXT NOT NULL UNIQUE,       -- hex SHA-256 of the raw token bytes
  created_at     INTEGER NOT NULL,
  created_by     TEXT NOT NULL,
  expires_at     INTEGER,                    -- NULL = never
  revoked_at     INTEGER,
  allow_download INTEGER NOT NULL DEFAULT 1,
  view_count     INTEGER NOT NULL DEFAULT 0,
  last_viewed_at INTEGER
);
CREATE INDEX shares_by_item ON shares (item_id);
```

Run `PRAGMA foreign_keys = ON` semantics explicitly: D1 enforces foreign keys by default, but
the delete route must still delete shares in the same `DB.batch` so behaviour does not depend
on it.

Object keys never contain user input. The extension comes from an allowlist keyed by
content type (section 6), not from the filename.

## 5. HTTP API

All bodies are JSON. Errors use one envelope: `{"error":{"code":"…","message":"…"}}` with a
stable `code` (`unauthorized`, `not_found`, `invalid_request`, `too_large`,
`unsupported_type`, `conflict`, `upload_incomplete`, `internal`). Never echo SQL, stack
traces, object keys of other items or signing material.

### 5.1 Authenticated (`APP_HOST`, prefix `/api`)

| Method & path | Request | Response |
|---|---|---|
| `GET /api/me` | – | `{identity, kind:"user"\|"service", limits:{max_upload_bytes, multipart_threshold, part_size}}` |
| `POST /api/uploads` | `{filename, size, content_type, kind, title?, captured_at, width?, height?, duration_ms?, thumbnail?:bool}` | `201 {item_id, upload, thumbnail_upload?}` |
| `POST /api/uploads/:id/parts` | `{part_numbers:[1,2,…]}` (max 32) | `{parts:[{part_number,url,expires_at}]}` |
| `POST /api/uploads/:id/complete` | `{parts?:[{part_number,etag}]}` | `{item}` |
| `DELETE /api/uploads/:id` | – | `204`; aborts multipart, deletes objects and the pending row |
| `GET /api/items` | `?limit=1..100&cursor=&q=&kind=&favorite=1` | `{items:[…], next_cursor}` |
| `GET /api/items/:id` | – | `{item, shares:[…]}` |
| `PATCH /api/items/:id` | `{title?, favorite?}` | `{item}` |
| `DELETE /api/items/:id` | – | `204`; deletes object, thumbnail, shares, row |
| `GET /api/items/:id/media` | `Range` honoured; `?download=1` | bytes |
| `GET /api/items/:id/thumb` | – | JPEG, `Cache-Control: private, max-age=86400` |
| `POST /api/items/:id/shares` | `{expires_in_seconds?: number\|null, allow_download?: bool}` | `201 {share, url}` — the only time the token is returned |
| `GET /api/items/:id/shares` | – | `{shares:[…]}` (no tokens) |
| `DELETE /api/shares/:id` | – | `204`; sets `revoked_at` |

`upload` is one of:

```jsonc
{"mode":"single","url":"https://…","headers":{"content-type":"video/mp4"},"expires_at":…}
{"mode":"multipart","part_size":33554432,"part_count":42}
```

Rules:

- `POST /api/uploads` validates: `size` in `1..=MAX_UPLOAD_BYTES`; `content_type` in the
  allowlist; `filename` ≤ 255 bytes, no control characters; `title` defaults to the filename
  stem. It inserts a `pending` row, then presigns. For multipart it calls `CreateMultipartUpload`
  and stores `upload_id`. `part_size` = `max(32 MiB, ceil(size / 9000))` rounded up to a MiB.
- Single PUT URLs live 15 minutes, part URLs 60 minutes. The client asks for part URLs in
  batches as it goes, so a slow upload never holds an expired URL.
- `complete`: for multipart, send `CompleteMultipartUpload` with the client's ETags. Then
  `BUCKET.head(object_key)`; if the object is missing return `409 upload_incomplete`; if its
  size differs from the declared `size`, delete it and the row and return `409`. On success
  set `status='ready'`, clear `upload_id`, set `thumb_key` if `BUCKET.head(thumb)` exists and
  is ≤ 1 MiB (delete an oversized thumbnail). `complete` on an already-ready item returns the
  item (idempotent, so a retried request after a dropped response succeeds).
- Only `ready` items appear in `GET /api/items` and can be shared.
- Listing is keyset-paged on `(captured_at DESC, id DESC)`; the cursor is base64url of
  `captured_at:id`. `q` matches `title` or `filename` with `LIKE '%' || ? || '%' ESCAPE '\'`
  (escape `%`, `_`, `\` in the input). Bind every value; never interpolate into SQL.
- Item JSON never includes `object_key`, `thumb_key` or `upload_id`. It includes
  `media_url` and `thumb_url` as relative API paths.

### 5.2 Public (`SHARE_HOST`)

| Path | Behaviour |
|---|---|
| `GET /s/:token` | HTML page: title, player (`<video controls>` / `<img>`), download button if allowed. Open Graph tags (`og:title`, `og:image` = thumb, `og:video` for mp4) so links unfurl. |
| `GET /s/:token/media` | Range-capable stream. `?download=1` only when `allow_download`. |
| `GET /s/:token/thumb` | JPEG thumbnail. |

Unknown, expired and revoked tokens all return the same `404` page. Token lookup: decode
base64url (reject anything that is not exactly 43 chars of the alphabet before touching D1),
SHA-256, `SELECT … WHERE token_hash = ?`. Increment `view_count` only on the page route, via
`ctx.waitUntil`, so a failed counter update never fails a view.

Every other path on `SHARE_HOST`, including `/api/*` and static assets, returns `404`.

### 5.3 Hostname dispatch (`src/server.ts`)

The Worker entry wraps the TanStack Start handler; nothing reaches Start, the API or a
static asset without passing through it.

```
host == SHARE_HOST → handlePublic: /s/* only, everything else 404
                     (never ASSETS, never the Start handler)
host == APP_HOST   → non-GET/HEAD: Origin check (403)
                   → verifyAccess (401)
                   → /api/*            → hono API (server/api.ts), identity attached
                   → GET/HEAD asset    → env.ASSETS.fetch(request) when it is not a 404
                   → everything else   → startHandler.fetch(request)
                                         (SSR pages and /_serverFn/* server functions)
dev/test only      → /__local/* → LocalSigner handler
anything else      → 404
```

Compare against `new URL(request.url).hostname`, never the `Host` header. The same module
exports `scheduled` for the cron cleanup.

## 6. Security requirements

These are acceptance criteria, each with a test.

1. **Access JWT** (`auth/access.ts`): read `Cf-Access-Jwt-Assertion`; verify with `jose`
   `jwtVerify` against `createRemoteJWKSet(new URL(ACCESS_TEAM_DOMAIN + "/cdn-cgi/access/certs"))`,
   `issuer: ACCESS_TEAM_DOMAIN`, `audience: ACCESS_AUD`, algorithms `["RS256"]`. Identity is
   `payload.email`, or `payload.common_name` for service tokens. Missing header, bad signature,
   wrong `aud`/`iss`, expired → `401`. Missing `ACCESS_TEAM_DOMAIN`/`ACCESS_AUD` → `500` at
   config validation; there is no "auth disabled" default. The JWKS resolver is injected so
   tests can supply a local key set.
2. **Dev bypass**: only when `ENVIRONMENT` is `dev` or `test` **and** the request hostname is
   `localhost`/`127.0.0.1`, the header `X-Dev-Identity` stands in for the JWT. A test asserts
   the bypass is ignored when `ENVIRONMENT=production`.
3. **CSRF**: every non-`GET`/`HEAD` request on `APP_HOST` — the API and the server functions
   alike — is rejected with `403` if it carries an `Origin` header that is not
   `https://APP_HOST`. Service-token calls send no `Origin` and pass. In addition,
   `POST`/`PATCH`/`DELETE` under `/api` with a body require `Content-Type: application/json`.
   No state-changing server function may use `GET`.
4. **Content types**: allowlist `image/png`, `image/jpeg`, `image/webp`, `image/gif`,
   `video/mp4`, `video/webm`, `video/x-matroska`, `video/quicktime`. Each maps to a fixed
   extension. Media responses send the stored type, `X-Content-Type-Options: nosniff`,
   `Content-Security-Policy: default-src 'none'; sandbox`, and `Content-Disposition` built
   with RFC 5987 `filename*` encoding (`inline`, or `attachment` when `download=1`).
5. **Share page**: HTML-escape every interpolated value (title, filename). Headers:
   `Content-Security-Policy: default-src 'none'; img-src 'self'; media-src 'self'; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'`,
   `Referrer-Policy: no-referrer`, `X-Robots-Tag: noindex, nofollow`,
   `Cache-Control: private, no-store`. No scripts on the page.
6. **Secrets and logs**: never log tokens, presigned URLs, JWTs or service-token headers.
   Presigned URLs are returned only to authenticated callers.
7. **Range handling** (`media.ts`): parse a single `bytes=a-b`, `bytes=a-` or `bytes=-n`
   range; pass `{range}` to `BUCKET.get`; reply `206` with `Content-Range`, or `416` with
   `Content-Range: bytes */size` when unsatisfiable. Always send `Accept-Ranges: bytes` and
   `ETag`. Multi-range requests fall back to a full `200`.
8. **Rate limiting** on `SHARE_HOST` (Workers rate-limiting binding, keyed by client IP,
   60 req/min on the page route) is a stretch goal; 256-bit tokens make guessing infeasible
   without it.

## 7. Storage layer

```ts
interface UploadSigner {
  presignPut(key: string, contentType: string, ttlSeconds: number): Promise<string>;
  createMultipart(key: string, contentType: string): Promise<string>;           // uploadId
  presignPart(key: string, uploadId: string, partNumber: number, ttl: number): Promise<string>;
  completeMultipart(key: string, uploadId: string, parts: {partNumber: number; etag: string}[]): Promise<void>;
  abortMultipart(key: string, uploadId: string): Promise<void>;
}
```

- `r2_s3.ts` (production): `new AwsClient({accessKeyId, secretAccessKey, service: "s3", region: "auto"})`
  against `https://<R2_ACCOUNT_ID>.r2.cloudflarestorage.com/<bucket>/<key>`. Presign with
  `client.sign(new Request(url, {method: "PUT", headers}), {aws: {signQuery: true}})` and
  `X-Amz-Expires` in the query. Sign `content-type` so the client cannot change it. Create,
  complete and abort are ordinary signed S3 requests (`POST ?uploads`,
  `POST ?uploadId=`, `DELETE ?uploadId=`); parse the XML with a small regex for `<UploadId>`
  and `<Code>`. Do **all** multipart control through the S3 API — do not mix it with the
  binding's multipart methods. Presigned URLs only work on the `r2.cloudflarestorage.com`
  endpoint, never on a custom domain.
- `local.ts` (dev/test only; constructing it throws when `ENVIRONMENT=production`): returns
  URLs back to the Worker (`/__local/put/<key>?exp=&sig=`, HMAC-SHA-256 with
  `LOCAL_SIGNING_KEY`), whose handler writes through the binding and returns an `ETag`
  header; multipart uses the binding's `createMultipartUpload` / `resumeMultipartUpload`.
  This exists because Miniflare has no S3 endpoint; it is what makes the Worker tests and the
  Rust end-to-end test hermetic.
- Reads, `head` and deletes always use the `BUCKET` binding.

Cron cleanup (`scheduled`): for `pending` rows older than 24 h — abort the multipart upload
if any, delete `object_key` and the thumbnail key, delete the row. Process at most 200 rows
per run. Errors on one row are logged (ids only) and do not stop the batch.

## 8. Rust client — `crates/cloud`

Dependencies: `upload = { workspace = true, default-features = false }`, `reqwest` and
`tokio` at the same versions and features `upload` uses, `serde`, `serde_json`, `thiserror`,
`tokio-util`, `url`. Add `cloud = { path = "crates/cloud" }` to `[workspace.dependencies]`.
`reqwest::Client::new()` panics in this workspace — always start from
`upload::http::client_builder()`.

```rust
pub struct CloudConfig { pub base_url: Url, pub client_id: String, pub client_secret: String }
pub struct NewUpload { pub path: PathBuf, pub kind: ItemKind, pub title: Option<String>,
    pub captured_at_ms: i64, pub width: Option<u32>, pub height: Option<u32>,
    pub duration_ms: Option<u64>, pub thumbnail_jpeg: Option<Bytes> }
pub struct CloudClient { /* http, config */ }
impl CloudClient {
    pub fn new(config: CloudConfig) -> Result<Self, CloudError>;
    pub async fn me(&self) -> Result<Me, CloudError>;
    pub async fn upload(&self, new: NewUpload, progress: Arc<dyn ProgressSink>,
        cancel: CancellationToken) -> Result<Item, CloudError>;
    pub async fn list(&self, query: &ListQuery) -> Result<Page, CloudError>;
    pub async fn create_share(&self, item_id: &str, expires_in: Option<Duration>) -> Result<Share, CloudError>;
    pub async fn revoke_share(&self, share_id: &str) -> Result<(), CloudError>;
    pub async fn delete_item(&self, item_id: &str) -> Result<(), CloudError>;
}
```

Behaviour:

- `base_url` must be `https` (allow `http` only for `localhost`/`127.0.0.1`, for tests). Send
  the two `CF-Access-Client-*` headers on API calls **only** to `base_url`'s origin; never
  attach them to presigned R2 URLs. Disable redirects on the API client: an Access login
  redirect (`302` to `*.cloudflareaccess.com`) means the service token was rejected — map it
  to `CloudError::Auth` with a message naming the token, not a JSON parse error.
- `Debug` for `CloudConfig` redacts `client_secret`.
- Single PUT: stream the file with `upload::body::BodyPlan::raw` (exact `Content-Length`,
  64 KiB chunks), with the headers from the `upload` response.
- Multipart: sequential parts (no concurrency in v1). Per part: seek, stream exactly
  `part_size` bytes, read the `ETag` response header, retry up to 4 times with backoff on
  network errors and 5xx, re-requesting a fresh URL after a `403`. Request part URLs 8 at a
  time. Progress reports total bytes across parts and must not go backwards on a retry.
- On any failure or cancellation after `POST /api/uploads`, best-effort
  `DELETE /api/uploads/:id` (bounded to 10 s, not cancellable by the same token).
- The thumbnail is a second small presigned PUT sent before `complete`; its failure is
  logged and ignored.
- If the file's size changes between the initial `stat` and the end of the stream, fail with
  `CloudError::SourceChanged` (a capture still being written).

Tests (`wiremock`, as in `crates/upload/tests`): single PUT happy path with header and body
assertions; multipart with 3 parts and ETag round-trip; part retry after a 500; fresh URL
after a 403; cancellation mid-part triggers the abort call; Access redirect maps to `Auth`;
credentials are not sent to the presigned host; secret absent from `Debug` output.

## 9. Desktop integration

Follow the existing ownership rule from `docs/DEVELOPMENT.md`: workers own long-running
work, UI state changes happen on the UI thread, nothing in `view` touches disk or network.

### 9.1 `crates/desktop/src/cloud.rs`

- One named thread `kielbox-cloud` owning a current-thread Tokio runtime (the pattern in
  `updates.rs`), fed by a command channel and reporting through an `mpsc` receiver plus
  `rune::Waker`, like `rune_thumbs.rs`. Commands: `Test`, `Upload(path)`, `Cancel(path)`,
  `Share(path)`, `Revoke(path)`, `Shutdown`. Events: `Tested(Result)`, `Progress{path, sent, total}`,
  `Uploaded{path, item_id}`, `Shared{path, url}`, `Failed{path, message}`.
- One upload at a time; further requests queue in order.
- Local state file `cloud.json` in the working directory (next to `preferences.json`, written
  atomically via temp file + rename, read once at startup on the worker):
  `{version: 1, items: {<path>: {item_id, size, modified_ms, share?: {id, url}}}}`.
  An entry counts as uploaded only while the file's size and mtime still match. Do not put
  this in `preferences.json`; `preferences_io.rs` has its own barrier/ack protocol that
  should not gain a second writer.
- Credentials: `toolkit_services::LayeredSecretStore::open()` with keys
  `kielbox-cloud-client-id` and `kielbox-cloud-client-secret`. Add `toolkit-services` and
  `cloud` as direct dependencies of `desktop` (`toolkit-services` is already in its graph via
  `toolkit`). Never write the secret to `preferences.json`, logs or notices.
- Thumbnail: reuse the on-disk cache from `rune_thumbs.rs` (expose a `pub(crate)` function
  returning the cached RGBA + duration for a path) and encode it as JPEG quality 80 with the
  `image` crate (enable its `jpeg` feature for `desktop`). No thumbnail if none is cached.
- `kind` mapping follows the library filters in `rune_ui.rs::shown` (Replays, Recordings,
  Screenshots); anything else is `other`.
- Shutdown: an in-flight upload must not block quit. `prepare_shutdown` sends `Shutdown`,
  which cancels the token, attempts the abort call and joins within 10 s. Do not add uploads
  to `shutdown_blocked`.

### 9.2 Preferences

Add to `Preferences` with serde defaults so existing files load unchanged:
`cloud_url: String` (empty = disabled), `cloud_auto_upload: bool` (default false),
`cloud_share_days: u32` (default 7; 0 = never expires). Cover the round-trip and the
old-file-loads cases in the existing preferences tests.

### 9.3 UI (`rune_cloud.rs`, wired from `rune_ui.rs`)

- Settings: add a sixth tab **Cloud** to `SETTINGS_TABS`: URL field, Client ID field, Client
  secret field (masked; shows "Saved in <store name>" once stored, with a Replace button),
  **Test connection** (calls `/api/me`, shows identity or the error), auto-upload switch,
  share-expiry choice (1 day / 7 days / 30 days / Never), **Open web library** button.
- Library card (`clip_card`): one extra icon button before Favorite, only when a cloud URL is
  configured. States: not uploaded → "Upload to cloud"; queued/uploading → progress percent,
  press to cancel; uploaded → "Copy share link" (creates the share on first use, then copies
  the stored URL; reuses `recordings::write_clipboard_owned` on Windows); failed → retry,
  with the message in a notice. Mirror the same control in list view.
- Auto-upload: when enabled, enqueue files that newly appear from `JobResult::Capture` /
  `Saved` and finished recordings. Never bulk-upload the existing library on enable.
- Add a `rune_fixtures.rs` scene for the Cloud settings tab and a card in each state so the
  Wine render step (`scripts/ci/build-windows-cross.sh render`) covers them.
- Non-Windows builds: the module compiles and the settings tab renders; clipboard copy shows
  the URL in a notice instead.

## 10. Web explorer (TanStack Start)

Served only on `APP_HOST`, after the gate in `src/server.ts`. React, TanStack Router (file
routes), TanStack Query, Tailwind CSS v4 and Base UI.

UI rules:

- Primitives come from Base UI (`@base-ui/react`; confirm the current package name and
  install steps in the Base UI docs — it was `@base-ui-components/react` before 1.0). Base UI
  is unstyled: all styling is Tailwind classes written in this repo. Do not add shadcn/ui,
  Radix or any other component library.
- Wrap each primitive once in `src/components/ui/` and use only the wrappers elsewhere, so
  styling lives in one place: `dialog.tsx`, `alert-dialog.tsx`, `menu.tsx`, `select.tsx`,
  `switch.tsx`, `tooltip.tsx`, `toast.tsx`, `toggle-group.tsx`, `field.tsx` (Field + Input).
  `button.tsx`, `badge.tsx` and `skeleton.tsx` are plain styled elements with no primitive.
  Style states through Base UI's data attributes (`data-open`, `data-highlighted`,
  `data-checked`, `data-starting-style` / `data-ending-style` for transitions), not React state.
- Follow Base UI's setup notes: `isolation: isolate` on the app root so portalled popups
  stack correctly, one `Toast.Provider` and one `Tooltip.Provider` in `__root.tsx`.
- Icons: `lucide-react`. No animation library; CSS transitions only.
- Dark theme only. Define the Quiet design tokens from `crates/desktop/src/rune_theme.rs`
  (background, surface, line, text levels, accent, radii) as CSS variables in a Tailwind
  `@theme` block in `styles.css` and use only those colours, so the web library reads as the
  same product as the desktop app. No theme switcher.
- App components (`Card`, `Grid`, `FilterChips`, `Player`, `SharePanel`) live in
  `src/components/` and compose the `ui/` wrappers.
- Use the alert dialog for delete and revoke confirmations, toasts for mutation results and
  "link copied", skeleton tiles while a page loads.
- Self-host the font the desktop uses if its license allows (see `assets/fonts` and
  `THIRD_PARTY.md`); otherwise use the system UI stack. No third-party font or script hosts.
- The share page (section 5.2) does **not** use Tailwind or Base UI: it keeps a small inline
  stylesheet using the same token values.

Server functions (`src/functions/`):

- `functions/auth.ts` exports `authMiddleware` (a function middleware whose `.server()` step
  takes the current request, calls the same `verifyAccess` as the entry, and adds
  `identity` to the context) and `authedFn = createServerFn().middleware([authMiddleware])`
  style factories for `GET` and `POST`. **Every** server function is built from `authedFn`.
  A test greps `src/` and fails if `createServerFn(` appears outside `functions/auth.ts`.
  This is the second layer behind the entry gate, so a future routing change cannot expose a
  function unauthenticated.
- Functions are thin: validate input with a zod schema (`.inputValidator`), call one
  function in `src/server/services/`, return its plain-JSON result. Reads use `GET`, mutations
  `POST`. Bindings come from `import { env } from "cloudflare:workers"`.
- `listItems`, `getItem`, `updateItem`, `deleteItem`, `createShare`, `listShares`,
  `revokeShare`, `me`. There are no upload functions: the explorer does not upload in v1.
- Nothing under `src/server/` may be imported from a route or component except through
  `src/functions/`; this keeps signing keys and SQL out of the client bundle. Add the
  `*.server.ts` naming or an import-protection rule if the Start version provides one.

Routes:

- `/` — library grid. Search params validated with `validateSearch`: `q`, `kind`,
  `favorite`. Cards show thumbnail, title, date, duration or size and a kind badge, newest
  first, grouped by month. `useInfiniteQuery` on `next_cursor`, loading the next page from an
  intersection observer. The loader prefetches the first page so it is server-rendered.
  Search box debounced 250 ms; filter chips match the desktop (All / Replays / Recordings /
  Screenshots / Favorites) and live in the URL.
- `/items/$itemId` — detail. `<video controls>` or `<img>` pointing straight at
  `/api/items/:id/media` (same origin, so the Access cookie applies and `Range` seeking works
  without any client code). Rename, favorite, download, delete with a confirm dialog. Shares
  panel: create (expiry choice, allow download), list with created / expires / views, revoke,
  copy link. The share URL is shown once, right after creation.
- Not-found and error components on the root route. A `401` from any server function
  triggers a full page reload so Access can re-authenticate.

Rules:

- Mutations go through `useMutation` and invalidate the affected query keys; favorite and
  rename update optimistically.
- Never use `dangerouslySetInnerHTML`.
- Explorer `Content-Security-Policy`, set in `src/server.ts` on HTML responses:
  `default-src 'self'; img-src 'self' blob: data:; media-src 'self'; style-src 'self' 'unsafe-inline'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'`
  (Base UI popups set inline styles for positioning, hence `style-src 'unsafe-inline'`),
  with `script-src 'self' 'nonce-<per-request>'` if the installed Start version supports an
  SSR nonce, otherwise `script-src 'self' 'unsafe-inline'` (Start injects inline hydration
  scripts). The fallback is acceptable here because this origin never serves user-authored
  HTML and all media responses carry `sandbox`.

## 11. Milestones

Each milestone lists what must be true before moving on.

**M1 — App skeleton, config and auth.**
Scaffold the TanStack Start app for Workers (section 2), `server/env.ts` validation, the
`src/server.ts` dispatch, Access verification, origin check, error envelope, `GET /api/me`,
a placeholder index route that shows the signed-in identity through a server function,
migration `0001`.
Done when: `npm run typecheck`, `npm test` and `npm run build` pass; tests cover valid JWT,
wrong `aud`, wrong issuer, expired, missing header, service-token identity, dev bypass refused
in production, `/api/*`, `/_serverFn/*` and `/` on the share host → 404, unknown host → 404,
a cross-origin `POST` on the app host → 403.

**M2 — Uploads and items.**
`UploadSigner` with both implementations, upload routes, item list/get/patch/delete, media
and thumbnail streaming, cron cleanup.
Done when: tests (local signer) cover single upload → complete → list → ranged read
(`200`, `206`, suffix range, `416`); multipart with 3 parts; size mismatch rejected and
cleaned up; disallowed type and oversize rejected; paging returns every item exactly once
across pages; `LIKE` wildcards in `q` are escaped; delete removes both objects; cron removes
stale pending rows only. A unit test checks a presigned R2 URL from `r2_s3.ts` for the
expected host, `X-Amz-Expires`, `X-Amz-SignedHeaders` containing `content-type`, and that it
is deterministic for a fixed clock.

**M3 — Shares.**
Share create/list/revoke, public routes, share page.
Done when: tests cover token returned once and not stored (D1 holds only the hash); expired
and revoked tokens → 404; malformed token → 404 without a D1 query; download refused when
`allow_download` is false; deleting an item kills its shares; title containing
`<script>` is escaped in the page; all headers from section 6.5 present.

**M4 — Web explorer.**
Routes, server functions, components and styles from section 10.
Done when: `npm run build` succeeds; the guard test proving every server function goes
through `authedFn` passes; with `npm run dev` and the dev bypass, an item uploaded by a test
script can be browsed, searched, renamed, favorited, shared, opened through its share URL in
a private window, revoked (share URL then 404s) and deleted; a hard reload on
`/items/<id>` server-renders the item. Record this as a manual checklist in `docs/CLOUD.md`.

**M5 — Rust client.**
`crates/cloud` with the wiremock tests from section 8, plus one `#[ignore]` end-to-end test
that runs against `npm run dev` (`KIELBOX_CLOUD_TEST_URL`), uploads a 70 MiB sparse file
through multipart and reads it back by range.
Done when: `cargo test --locked -p cloud -j1` and
`cargo clippy --locked -p cloud --all-targets -- -D warnings` pass.

**M6 — Desktop.**
Worker thread, `cloud.json`, preferences, Settings tab, card control, auto-upload, fixtures.
Done when: `cargo test --locked -p desktop -j1` passes including new tests for state-file
round-trip, stale-entry detection (size/mtime change), queue ordering, cancel, and shutdown
with an upload in flight (against a wiremock server that stalls); the Wine render shows the
Cloud tab and card states.

**M7 — Deployment and docs.**
`docs/CLOUD.md`: create bucket, D1 and R2 token; Access application, policies and service
token; `wrangler deploy` and `wrangler d1 migrations apply --remote`; desktop setup; rotation
(new R2 token, new service token); how to revoke all shares
(`UPDATE shares SET revoked_at = …`); cost notes; the M4 checklist. Update `README.md` (the
"upload destinations … not wired in Rune" line), `docs/RUNE_MIGRATION.md`, `THIRD_PARTY.md`
(TanStack Start/Router/Query, React, Tailwind, Base UI, lucide-react, hono, zod, aws4fetch, jose and any new crates) and add a
row to `plans/implementation-tracker.md`. Add a CI job that runs
`npm ci && npm run typecheck && npm test && npm run build` in `cloud/` on pull requests
touching `cloud/**`.
Done when: a real deployment passes this smoke test, recorded in the tracker — unauthenticated
`curl https://cloud.<domain>/api/me` is redirected by Access; the same call with the service
token returns the identity; a call with a forged `Cf-Access-Jwt-Assertion` straight to the
Worker is `401`; a 2 GiB recording uploads from the desktop, plays with seeking in the
explorer and through a share link; revoking the share makes the link 404 at once.

## 12. Out of scope for v1

Per-user libraries or roles; in-app browsing or downloading of cloud-only items; two-way
sync and deletion propagation; resumable multipart across app restarts; parallel part
uploads; server-side transcoding or thumbnail generation; albums and multi-item shares;
password-protected shares; client-side encryption; content deduplication; storage quotas
beyond `MAX_UPLOAD_BYTES`; browser uploads from the explorer.

## 13. Open questions for the owner

Defaults are chosen so work is not blocked; changing one later is contained.

1. **Two hostnames** (`cloud.` + `share.`) are assumed. A single hostname with an Access
   *Bypass* application on `/s/*` also works but puts shared media on the Access cookie's
   origin; only sections 3 and 5.3 change.
2. **Single tenant** is assumed. Multi-user needs an `owner` column and filters on every
   query — cheap to add before data exists, awkward after.
3. **"Library exploration"** is read as the web explorer plus per-card cloud state in the
   desktop library. Browsing cloud-only items inside the desktop app is deferred.
4. **Default share expiry** is 7 days. **Maximum upload** is 20 GiB.
5. Uploads send the original file. Uploading the "Prepare MP4" H.264 output instead (smaller,
   plays in every browser — the app records AV1) is a natural follow-up; AV1 in MP4 will not
   play in older Safari.

## Implementation notes

Written after M1-M7 (documentation part). Where the plan and the code differ, the code and
`docs/CLOUD.md` are right.

### Server (`cloud/`)

- `POST /api/items/:id/shares`: omitting `expires_in_seconds` means 7 days; an explicit `null`
  means never expires. (The plan left the omitted case open.) The minimum is 60 seconds.
- The test package is `@cloudflare/vitest-plugin` (the plan named the older
  `@cloudflare/vitest-pool-workers`). Server functions use `.validator(...)` where the plan
  says `.inputValidator`. Base UI is `@base-ui/react` 1.9.0.
- Local dev is `cd cloud && npm run dev` on `http://localhost:8787`, selecting the `dev`
  environment of `wrangler.jsonc`. App host is `localhost`, share host is `127.0.0.1`; the
  header `X-Dev-Identity` stands in for the Access JWT (an identity without `@` is a service
  token). Details in `cloud/README.md`.
- `wrangler.local.jsonc` is not wired up. The Vite build bakes `wrangler.jsonc` into
  `dist/server/wrangler.json`, which `wrangler deploy` uses, so real ids and hostnames must be
  in `wrangler.jsonc` at build time. `docs/CLOUD.md` tells the owner to edit it locally and
  mark it `skip-worktree`. The custom-domain `routes` are not in the tracked file either.
- Missing required variables make every request answer `500 server misconfigured`; they do not
  fail at deploy time.

### Desktop (`crates/desktop`)

- Copying and opening links use Rune's own `Tx::copy` and `Tx::open_url`, on all platforms.
  The plan's `recordings::write_clipboard_owned` needs a window handle nothing provides and has
  no callers, so it is not used, and there is no "show the URL in a notice instead" fallback
  (the notice shows the link in addition).
- The Library has no list layout (the `list_view` preference exists but nothing renders it), so
  the cloud button exists on grid cards only.
- The worker's command set gained `Configure { url, share_days }` (the worker owns no UI
  state), and `Test` carries optional typed credentials. Typed credentials are saved to the
  secret store only after `/api/me` succeeds. Events gained `Loaded`, `Credentials`, `Started`,
  `Cancelled`, `Stale` and `Warning`; `Failed` is kept for errors. `Revoke` is implemented and
  tested in the worker but no button sends it yet.
- The state file is `projects/cloud.json` (next to `projects/preferences.json`, which is where
  that file really lives, not the working directory root). A corrupt or newer-version file is
  renamed to `cloud.json.unreadable` rather than overwritten. Share records also store the
  server's `expires_at`; an expired or nearly expired stored link is replaced, not copied.
- Auto-upload does not hook `JobResult::Capture` / `Saved` (the latter carries no path).
  After every library scan the UI diffs the file list against the previous scan and queues
  files that are new, modified within the last 10 minutes and not manually imported. The first
  scan only records a baseline, so enabling the switch never uploads existing files.
- No width or height is sent (decoding the original would cost more than the metadata is
  worth), and no title (the server defaults it to the file name stem, so display renames in
  Kielbox do not carry over). Duration and thumbnail come from the existing thumbnail cache.
- Shutdown: `prepare_shutdown` stops the worker only when nothing else blocks quitting. The
  worker gives the abort call 9 s; the UI waits at most 10 s and then abandons the thread. The
  worker restarts lazily if the quit does not happen.
- `desktop` now depends on `cloud`, `toolkit-services`, `upload`, `bytes`, `tokio` (all
  platforms), `tokio-util` and `url`, and enables `image`'s `jpeg` feature.
- Two fixture scenes, `cloud-settings-fixture` and `cloud-fixture`, were added and appended to
  the scene lists of `scripts/ci/build-windows-cross.sh`. They are synthetic (no media). They
  could not be rendered on the Linux build machine (no GPU adapter) and the Wine step was not
  run; the scene logic is covered by a headless runtime test.

### Process

- CI: `.github/workflows/cloud.yml` runs `npm ci`, typecheck, test and build in `cloud/` on pull
  requests touching `cloud/**`.
- Not done, by design: `wrangler deploy`, any `--remote` command, and the M7 smoke test. There
  was no Cloudflare account. The two manual checklists in `docs/CLOUD.md` are unticked.
