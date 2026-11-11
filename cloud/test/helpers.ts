import { env as rawEnv } from 'cloudflare:workers'
import { exportJWK, generateKeyPair, SignJWT, createLocalJWKSet, type JWK, type JWTVerifyGetKey } from 'jose'
import { dispatch, type NextHandler } from '../src/server/dispatch'
import type { AppEnv } from '../src/server/env'

export const baseEnv = rawEnv as unknown as AppEnv

export function envWith(overrides: Partial<AppEnv> | Record<string, string | undefined>): AppEnv {
  return { ...baseEnv, ...overrides } as AppEnv
}

export const APP = 'http://localhost'
export const SHARE = 'http://127.0.0.1'
export const DEV = { 'x-dev-identity': 'tester@example.com' }
export const JSON_HEADERS = { 'content-type': 'application/json' }

export const fakeAssets = {
  fetch: async (request: Request) => {
    const path = new URL(request.url).pathname
    return path === '/assets/app.js'
      ? new Response('console.log("asset")', { headers: { 'content-type': 'text/javascript' } })
      : new Response('no asset', { status: 404 })
  },
} as unknown as Fetcher

export const START_MARKER = 'start-handler-reached'
export const fakeStart: NextHandler = () =>
  new Response(`<html><body>${START_MARKER}</body></html>`, { headers: { 'content-type': 'text/html' } })

export interface Pending {
  waits: Promise<unknown>[]
  ctx: ExecutionContext
}
export function makeCtx(): Pending {
  const waits: Promise<unknown>[] = []
  const ctx = {
    waitUntil: (p: Promise<unknown>) => void waits.push(p),
    passThroughOnException: () => {},
    props: {},
  } as unknown as ExecutionContext
  return { waits, ctx }
}

export interface CallOptions {
  env?: AppEnv
  jwks?: JWTVerifyGetKey
  next?: NextHandler
}

/** Drive the full Worker dispatch with a fake Start handler and fake assets. */
export async function call(request: Request, o: CallOptions = {}): Promise<Response> {
  const pending = makeCtx()
  const e = o.env ?? { ...baseEnv, ASSETS: fakeAssets }
  const response = await dispatch(request, { ...e, ASSETS: fakeAssets }, pending.ctx, {
    next: o.next ?? fakeStart,
    jwks: o.jwks,
  })
  await Promise.all(pending.waits)
  return response
}

export function app(path: string, init: RequestInit = {}): Request {
  const headers = new Headers(init.headers)
  if (!headers.has('x-dev-identity') && !headers.has('cf-access-jwt-assertion')) {
    headers.set('x-dev-identity', DEV['x-dev-identity'])
  }
  return new Request(`${APP}${path}`, { ...init, headers })
}

export function json(body: unknown, method = 'POST'): RequestInit {
  return { method, headers: JSON_HEADERS, body: JSON.stringify(body) }
}

/** Local RSA key pair and JWKS resolver, standing in for the Access certs endpoint. */
export async function makeAccessKeys() {
  const { publicKey, privateKey } = await generateKeyPair('RS256', { extractable: true })
  const jwk: JWK = { ...(await exportJWK(publicKey)), kid: 'k1', alg: 'RS256', use: 'sig' }
  const jwks = createLocalJWKSet({ keys: [jwk] })
  async function sign(
    claims: Record<string, unknown>,
    o: { iss?: string; aud?: string; exp?: string | number; key?: CryptoKey } = {},
  ) {
    return await new SignJWT(claims)
      .setProtectedHeader({ alg: 'RS256', kid: 'k1' })
      .setIssuer(o.iss ?? baseEnv.ACCESS_TEAM_DOMAIN)
      .setAudience(o.aud ?? baseEnv.ACCESS_AUD)
      .setIssuedAt()
      .setExpirationTime(o.exp ?? '5m')
      .sign(o.key ?? privateKey)
  }
  return { jwks, sign }
}

export function bytes(n: number, seed = 7): Uint8Array {
  const out = new Uint8Array(n)
  for (let i = 0; i < n; i++) out[i] = (i * 31 + seed) & 0xff
  return out
}

/** Create an upload, PUT the bytes to the signed local URL, complete it. Returns the item JSON. */
export async function uploadItem(
  opts: {
    filename?: string
    content_type?: string
    kind?: string
    title?: string
    captured_at?: number
    data?: Uint8Array
    thumbnail?: Uint8Array
  } = {},
) {
  const data = opts.data ?? bytes(1000)
  const create = await call(
    app('/api/uploads', json({
      filename: opts.filename ?? 'clip.mp4',
      size: data.length,
      content_type: opts.content_type ?? 'video/mp4',
      kind: opts.kind ?? 'clip',
      title: opts.title,
      captured_at: opts.captured_at ?? 1_700_000_000_000,
      thumbnail: opts.thumbnail ? true : undefined,
    })),
  )
  if (create.status !== 201) throw new Error(`create failed: ${create.status} ${await create.text()}`)
  const body = (await create.json()) as any
  await putLocal(body.upload.url, body.upload.headers, data)
  if (opts.thumbnail) await putLocal(body.thumbnail_upload.url, body.thumbnail_upload.headers, opts.thumbnail)
  const complete = await call(app(`/api/uploads/${body.item_id}/complete`, { method: 'POST' }))
  if (complete.status !== 200) throw new Error(`complete failed: ${complete.status} ${await complete.text()}`)
  return ((await complete.json()) as any).item as { id: string; [k: string]: any }
}

export async function putLocal(url: string, headers: Record<string, string>, data: Uint8Array) {
  const r = await call(new Request(url, { method: 'PUT', headers, body: data as BodyInit }))
  if (r.status !== 200) throw new Error(`local put failed: ${r.status} ${await r.text()}`)
  return r
}
