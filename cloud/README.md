# Kielbox Cloud (web and Worker)

TanStack Start app on Cloudflare Workers: presigned R2 uploads, a D1 index, the web library
explorer behind Cloudflare Access, and revocable share links. The design is in
`../plans/cloud.md`; this file covers working in `cloud/`.

```
npm ci
npm run typecheck && npm test && npm run build
```

## Layout notes

- `src/server.ts` is the Worker entry: every request goes through `src/server/dispatch.ts`
  (hostname split, Origin check, Access JWT verification) before TanStack Start, the API or a
  static asset sees it. It also exports `scheduled` (nightly cleanup of stale pending uploads).
- `src/server/**` is framework-free (hono, zod, jose, aws4fetch only). Tests target it with
  the Workers Vitest integration (`@cloudflare/vitest-plugin`: Miniflare D1 + R2).
- `src/functions/*` are the explorer's server functions; all are built from `authedGet` /
  `authedPost` in `functions/auth.ts` (checked by `test/guard.test.ts`).
- `wrangler.jsonc` holds placeholders only. Real ids, hostnames and Access values belong in
  your deployment (untracked `wrangler.local.jsonc` or `--var`), never in git.

## Local development

```
npm run dev      # applies the D1 migration to the local database, then starts Vite
```

The server listens on `http://localhost:8787` (and `http://127.0.0.1:8787`; Vite is bound to
127.0.0.1, port 8787 and `--strictPort`). `npm run dev` selects the `dev` environment of
`wrangler.jsonc` (`CLOUDFLARE_ENV=dev`), which sets:

| Variable | Value | Meaning |
|---|---|---|
| `ENVIRONMENT` | `dev` | enables the dev auth bypass and the local signer (both refuse `production`) |
| `APP_HOST` | `localhost` | the **app** host: Access-gated API, explorer, server functions |
| `SHARE_HOST` | `127.0.0.1` | the **share** host: serves only `/s/*` |
| `LOCAL_SIGNING_KEY` | throwaway | HMAC key for the local upload signer |

Dispatch compares the request URL's hostname (never the `Host` header), so the same port serves
both hosts: use `http://localhost:8787` for the app and `http://127.0.0.1:8787` for share
links. Share links returned by the API already use the share host and the request's scheme
and port.

Authentication: on a loopback hostname in `dev`/`test`, send `X-Dev-Identity: you@example.com`
in place of the Access JWT (an identity without `@` is treated as a service token). Without
it, every app-host request is `401`. The header is ignored when `ENVIRONMENT=production`.

Uploads: `POST /api/uploads` returns presigned URLs that point back at the Worker
(`http://localhost:8787/__local/put/...`, HMAC-signed, no auth headers needed). Single PUT
below 64 MiB, multipart at or above it (32 MiB parts), exactly as in production.

### Smoke test with curl

```
H='X-Dev-Identity: me@example.com'; B=http://localhost:8787
curl -s -H "$H" $B/api/me
curl -s -H "$H" -H 'content-type: application/json' $B/api/uploads \
  -d '{"filename":"a.mp4","size":5000,"content_type":"video/mp4","kind":"clip","captured_at":1700000000000}'
# PUT the bytes to upload.url with the returned headers, then:
curl -s -X POST -H "$H" $B/api/uploads/<item_id>/complete
curl -s -H "$H" -H 'Range: bytes=0-99' $B/api/items/<item_id>/media
curl -s -H "$H" -H 'content-type: application/json' -d '{}' $B/api/items/<item_id>/shares   # url on 127.0.0.1
curl -s http://127.0.0.1:8787/s/<token>                                                      # share page
curl -s -X DELETE -H "$H" $B/api/shares/<share_id>                                           # then the link is 404
curl -s -H "$H" $B/                                                                          # explorer, server-rendered
```

The explorer needs a browser session to click through; open `http://localhost:8787/` with a
header-injecting extension, or exercise the same operations through the API.

## Tests

`npm test` runs Vitest inside the Workers runtime with in-memory D1 and R2 and the local
signer; the migration is applied in `test/setup.ts`. Multipart sizes are shrunk in
`vitest.config.ts` (R2's 5 MiB part minimum still applies).

## Deploying

Not wired up here. See `docs/CLOUD.md` (written in milestone 7).

The authenticated library is served at `/`. The marketing site is a separate
Astro project in [`../site/`](../site/README.md), configured for `box.kieler.dev`.
Cloud has not been deployed; it still requires the setup in `docs/CLOUD.md`.

Local design verification on 11 November 2026 covered desktop/mobile rendering,
inline rename, favorite changes, link creation, revocation with a 404 response,
search and clear-search. These checks used a temporary image fixture that was
removed afterwards; they do not validate Cloudflare deployment or video seeking.
