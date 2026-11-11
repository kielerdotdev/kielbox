# Kielbox Cloud

Kielbox Cloud is a personal, single-tenant library on Cloudflare. The desktop uploads
captures to it, a web explorer browses them, and share links work without a login. The
design and the reasons for it are in [the plan](../plans/cloud.md); this file is how to set
it up and run it.

> Status: the code is built and tested locally. It has **not** been deployed to a real
> Cloudflare account, the web explorer has not been run in a browser, and the desktop side
> has not run on Windows. The two checklists at the end are empty on purpose. Treat every
> dashboard step below as written from documentation, not from a run, and expect labels in
> the Cloudflare dashboard to differ a little.

## How it fits together

- One Worker (`cloud/`) serves two hostnames. `cloud.<domain>` is the app: the explorer, the
  JSON API the desktop calls, and the server functions. It sits behind Cloudflare Access
  **and** the Worker checks the Access JWT itself on every request, so a missing or wrong
  Access application leaves the library closed (`401`), not open.
- `share.<domain>` is public and serves only `/s/<token>` pages and media. It has no Access
  application. It is a separate origin so a hostile shared file can never reach `/api` with
  your Access cookie.
- Uploads go from the desktop straight to R2 through presigned S3 URLs; the Worker never
  carries upload bodies. Downloads and share views stream through the Worker from the R2
  binding, so revoking a share takes effect at once.
- D1 holds the index (items and shares). Share tokens are stored only as SHA-256 hashes.
- The desktop authenticates with an Access **service token**, stored in the system
  credential store (Windows Credential Manager).

## Prerequisites

- A Cloudflare account with a domain whose DNS is on Cloudflare. Custom domains for a Worker
  need the zone there. You will use two hostnames on it, for example `cloud.example.com` and
  `share.example.com`.
- R2 enabled on the account. R2 needs a payment method on file.
- Cloudflare Zero Trust (Access) enabled, with a team name. Pick the free plan if it covers
  your users; check the current plan limits yourself.
- Node.js 22.12 or newer (Wrangler requires it; Vite requires 20.19 or newer) and npm.
- A checkout of this repository. Run all `npm` and `wrangler` commands in `cloud/`.

Log in once with `npx wrangler login`, or set `CLOUDFLARE_API_TOKEN` and
`CLOUDFLARE_ACCOUNT_ID` for non-interactive use.

## 1. Create the R2 bucket

```
npx wrangler r2 bucket create kielbox-cloud
```

The name must match `R2_BUCKET_NAME` and the `BUCKET` binding in `wrangler.jsonc`.

Add the lifecycle rule that aborts abandoned multipart uploads after one day. Without it,
the parts of an interrupted upload keep using storage:

```
npx wrangler r2 bucket lifecycle add kielbox-cloud abort-stale-multipart --abort-multipart-days 1
```

The same rule can be added in the dashboard under the bucket's Settings, Object lifecycle
rules. No CORS rule is needed: only the desktop uploads, and browsers never talk to the S3
endpoint.

The Worker also deletes pending uploads that are older than 24 hours once a night
(cron `17 3 * * *`), including their multipart state.

## 2. Create the D1 database

```
npx wrangler d1 create kielbox-cloud
```

Copy the `database_id` it prints. It goes into the Worker config in step 6. The schema is
`cloud/migrations/0001_init.sql`; it is applied in step 7.

## 3. Create the bucket-scoped R2 API token

The Worker signs upload URLs with an R2 API token. Make it as small as possible:

1. In the dashboard open R2, then **Manage API tokens**, then create an API token.
2. Permission: **Object Read & Write**.
3. Scope: **only the `kielbox-cloud` bucket**.
4. Create it and copy the **Access Key ID** and **Secret Access Key**. The secret is shown
   once.
5. Note your **account ID** (shown on the R2 overview page). It is `R2_ACCOUNT_ID`.

Do not use a Cloudflare account-wide API token for this, and do not create an "Admin" R2 token.

## 4. Protect the app hostname with Access

In the Zero Trust dashboard:

1. **Settings**: note your team domain, `https://<team>.cloudflareaccess.com`. It is
   `ACCESS_TEAM_DOMAIN`.
2. **Access, Service credentials (Service Auth)**: create a service token named
   `kielbox-desktop`. Copy the **Client ID** and **Client Secret** now; the secret is shown
   once. Tokens expire (the dashboard asks for a duration), so write down the date and see
   [rotation](#rotating-the-service-token).
3. **Access, Applications**: add a **Self-hosted** application.
   - Application domain: `cloud.<domain>`, all paths.
   - Policy 1: action **Allow**, include your own identity (your email, or your identity
     provider's group).
   - Policy 2: action **Service Auth**, include the `kielbox-desktop` service token.
   - Do not add a Bypass or Allow-everyone policy.
4. Open the application and copy its **Application Audience (AUD) Tag**. It is `ACCESS_AUD`.

Do **not** create an Access application for `share.<domain>`: share links must work for
people without a login. The Worker serves only `/s/*` on that hostname.

The Worker verifies `Cf-Access-Jwt-Assertion` against
`<ACCESS_TEAM_DOMAIN>/cdn-cgi/access/certs`, with the issuer and `ACCESS_AUD` you configure.
Service-token requests carry the same header; Access adds it at the edge.

## 5. Variables and secrets

These are the variables the Worker reads (`cloud/src/server/env.ts`). With any of the first seven
missing it answers every request `500 server misconfigured` (and logs the names, never the
values). There is no "auth disabled" default.

| Name | Kind | Value |
|---|---|---|
| `ENVIRONMENT` | var | `production`. The dev auth bypass and the local signer refuse to run with this value. |
| `APP_HOST` | var | `cloud.<domain>` (hostname only) |
| `SHARE_HOST` | var | `share.<domain>`; must differ from `APP_HOST` |
| `ACCESS_TEAM_DOMAIN` | var | `https://<team>.cloudflareaccess.com` |
| `ACCESS_AUD` | var | the application's AUD tag |
| `R2_ACCOUNT_ID` | var | your Cloudflare account ID |
| `R2_BUCKET_NAME` | var | `kielbox-cloud` |
| `MAX_UPLOAD_BYTES` | var, optional | default 20 GiB (`21474836480`) |
| `R2_ACCESS_KEY_ID` | **secret** | from step 3 |
| `R2_SECRET_ACCESS_KEY` | **secret** | from step 3 |

`LOCAL_SIGNING_KEY` exists only for local development and must not be set in production.
Uploads of 64 MiB and larger use multipart with parts of at least 32 MiB; both thresholds
have optional overrides (`MULTIPART_THRESHOLD_BYTES`, `PART_SIZE_MIN_BYTES`) that you
normally leave alone.

## 6. Put your values into the Worker config

`cloud/wrangler.jsonc` contains placeholders only and must stay that way in git. The Vite
build copies the vars, bindings and ids from that file into `dist/server/wrangler.json`,
and `wrangler deploy` deploys that generated file. So your real values have to be in
`wrangler.jsonc` **when you run the build**. The plan's idea of a separate
`wrangler.local.jsonc` is not wired up: nothing reads it. Instead edit `wrangler.jsonc` in
your working copy, and keep the edit out of commits:

```
git update-index --skip-worktree cloud/wrangler.jsonc
```

Change these in the top-level config (not the `env.dev` block):

- `d1_databases[0].database_id`: the id from step 2.
- `vars`: `APP_HOST`, `SHARE_HOST`, `ACCESS_TEAM_DOMAIN`, `ACCESS_AUD`, `R2_ACCOUNT_ID`.
- Add the two custom domains as routes:

```jsonc
"routes": [
  { "pattern": "cloud.example.com", "custom_domain": true },
  { "pattern": "share.example.com", "custom_domain": true }
]
```

Leave `workers_dev` and `preview_urls` at `false`. A `*.workers.dev` URL would sit outside
Access. (The Worker would still answer `401` there, but there is no reason to expose it.)

Use `git update-index --no-skip-worktree cloud/wrangler.jsonc` before pulling changes to
that file.

## 7. Deploy

```
cd cloud
npm ci
npm run typecheck && npm test && npm run build
npm run migrate:remote
npm run deploy
```

- `npm run migrate:remote` is `wrangler d1 migrations apply DB --remote`. It lists the
  pending migrations and asks for confirmation. Run it before the first deploy and again
  whenever a new file appears in `cloud/migrations/`.
- `npm run deploy` builds and runs `wrangler deploy`. The routes in step 6 create the two
  DNS records and certificates for the custom domains.

Then set the two secrets:

```
npx wrangler secret put R2_ACCESS_KEY_ID
npx wrangler secret put R2_SECRET_ACCESS_KEY
```

Uploads fail until both exist. `wrangler secret put` takes effect immediately; no redeploy
is needed.

## 8. Desktop setup

1. Open Kielbox, Settings, **Cloud**.
2. **Address**: `https://cloud.<domain>`.
3. **Client ID** and **Client secret**: from the `kielbox-desktop` service token.
4. **Test connection**. It calls `GET /api/me` and shows who you are connected as. On
   success the address is saved in `preferences.json` and the token is saved in the system
   credential store; the page then shows "Saved in Windows Credential Manager" with a
   Replace button. The secret is never written to `preferences.json`, logs or notices. If
   no credential store is reachable it is kept in memory for the session only, and the page
   says so.
5. Optional: **Upload new captures automatically** (off by default; only captures made while
   it is on, never your existing library), and how long new share links last (1 day, 7 days,
   30 days, never; default 7 days).

In the Library every card then has a cloud button: upload, then (once uploaded) copy share
link. The link is created on first use and the same link is copied afterwards, until it
expires. Uploads run one at a time; press the button while it shows a percentage to cancel.
Quitting cancels an upload in flight and asks the server to discard the half-finished item;
it does not keep Kielbox open.

What the desktop remembers lives in `projects/cloud.json` in the data folder, next to
`preferences.json`: for each uploaded file, the item id, the size and modification time at
upload, and the share link. A file counts as uploaded only while its size and modification
time still match, so editing an uploaded capture in place makes its card offer to upload it
again. The old item stays in the cloud. Deleting `cloud.json` is safe; Kielbox then forgets
what it uploaded.

The environment variables `SSX_SECRET_KIELBOX_CLOUD_CLIENT_ID` and
`SSX_SECRET_KIELBOX_CLOUD_CLIENT_SECRET` override the stored token, which is handy for a
headless machine.

## Rotation

### Rotating the R2 token

1. Create a new bucket-scoped token as in step 3.
2. `npx wrangler secret put R2_ACCESS_KEY_ID` and `... R2_SECRET_ACCESS_KEY` with the new
   values.
3. Check an upload from the desktop.
4. Delete the old token in the R2 API token list.

Presigned URLs already handed out are signed with the old key and stop working when it is
deleted. They live 15 minutes (single upload) or 60 minutes (a part), so an upload running
during rotation can fail once; retry it.

### Rotating the service token

1. In Zero Trust, create a second service token (or rotate the secret of the existing one,
   if the dashboard offers it).
2. Make sure the application's **Service Auth** policy includes the new token.
3. In Kielbox, Settings, Cloud: press **Replace**, enter the new Client ID and secret, and
   **Test connection**.
4. Remove the old token from the policy and delete it.

An expired or revoked token shows up in the desktop as an authentication failure naming the
token, not as a JSON error: Access answers with a login redirect, which the client does not
follow.

## Revoking all shares

Every link stops working the moment its `revoked_at` is set. To revoke all of them:

```
npx wrangler d1 execute DB --remote --command \
  "UPDATE shares SET revoked_at = CAST(strftime('%s','now') AS INTEGER) * 1000 WHERE revoked_at IS NULL"
```

To revoke the shares of one item, add `AND item_id = '<item id>'`. A single share can be
revoked from the web explorer or with `DELETE /api/shares/<id>`. Shares of a deleted item
are deleted with it.

## Cost

Costs depend on your Cloudflare plan and on how much you store, so this guide gives no
prices; use Cloudflare's current pricing pages.

- R2 charges for stored bytes and for operations, not for egress. Streaming media from the
  Worker uses R2 reads, not paid egress. Multi-gigabyte recordings dominate the bill, so
  deleting items you no longer need is the main lever.
- Workers and D1 usage here is small (a handful of requests per view, one small row per
  item); a personal library is far below any limit that matters. Check the current free-plan
  allowances for requests and D1 rows rather than relying on this guide.
- Zero Trust has a free tier for a small number of users; check the current limits.
- Failed uploads cost storage until the lifecycle rule or the nightly cleanup removes them;
  step 1 sets both up.

## Local development

See [`cloud/README.md`](../cloud/README.md): `npm ci`, `npm run dev`, and a curl walkthrough
against `http://localhost:8787` with the `X-Dev-Identity` header standing in for Access. The
dev environment uses throwaway values and refuses to run with `ENVIRONMENT=production`.

The desktop cannot be pointed at the dev server and used interactively: the dev server only
accepts the `X-Dev-Identity` header in place of Access, and the Settings page has no field
for it, so a desktop test connection against `http://localhost:8787` answers `401`. The
Rust end-to-end test in `crates/cloud/tests/e2e.rs` (ignored by default, set
`KIELBOX_CLOUD_TEST_URL`) covers the client against the dev server instead.

## Checks that have not been run

### Web explorer checklist (milestone 4) - not yet run

Run with `npm run dev` and a browser extension that adds `X-Dev-Identity: me@example.com`
to requests for `http://localhost:8787`. Upload one test item first (the curl walkthrough
in `cloud/README.md` does this). Tick each box when you have seen it work.

- [ ] The library grid shows the item with its thumbnail, title, date and kind badge,
      grouped by month.
- [ ] Searching (debounced) and each filter chip (All, Replays, Recordings, Screenshots,
      Favorites) change the list, and the URL reflects them.
- [ ] Scrolling to the end loads the next page.
- [ ] Opening the item plays the video with seeking, or shows the image.
- [ ] Rename and favorite update the card at once and survive a reload.
- [ ] Creating a share shows its URL once; opening it in a private window shows the share
      page with the player.
- [ ] A hard reload on `/items/<id>` server-renders the item.
- [ ] Revoking the share makes its URL return 404, and the shares panel reflects it.
- [ ] Deleting the item (with the confirm dialog) removes it from the grid and makes any
      remaining share URL return 404.
- [ ] A 401 from a server function (remove the header, act) reloads the page instead of
      leaving it broken.
- [ ] Nothing in the browser console is blocked by the Content-Security-Policy.

### Deployment smoke test (milestone 7) - not yet run

On a real deployment. Record the date and result of each line in
[the tracker](../plans/implementation-tracker.md).

- [ ] `curl -i https://cloud.<domain>/api/me` without credentials is redirected to Access
      (a `302` to `*.cloudflareaccess.com`), not answered with data.
- [ ] The same call with `-H "CF-Access-Client-Id: ..." -H "CF-Access-Client-Secret: ..."`
      returns the service-token identity and the limits.
- [ ] A forged `Cf-Access-Jwt-Assertion` is refused by the Worker itself with `401`. Access
      replaces that header at the edge, so to see the Worker's own answer, temporarily
      disable the Access application (the Worker then still returns `401` to every request
      without a valid token, so the library stays closed), send the request, and re-enable
      it.
- [ ] `https://share.<domain>/api/me` and `https://share.<domain>/` return 404.
- [ ] A 2 GiB recording uploads from the desktop, shows in the explorer, and plays with
      seeking there and through a share link.
- [ ] Revoking the share makes the link return 404 immediately.
- [ ] Quitting Kielbox during a large upload exits promptly, and the half-finished item does
      not appear in the library.
- [ ] An interrupted upload leaves no pending row after the nightly cleanup, and no
      incomplete multipart upload after a day.
