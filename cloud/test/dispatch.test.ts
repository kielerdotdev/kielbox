import { describe, expect, it } from 'vitest'
import { app, baseEnv, call, envWith, fakeStart, json, makeAccessKeys, START_MARKER } from './helpers'

const prodEnv = envWith({
  ENVIRONMENT: 'production',
  APP_HOST: 'cloud.example.com',
  SHARE_HOST: 'share.example.com',
  ACCESS_TEAM_DOMAIN: 'https://team.cloudflareaccess.com',
  R2_ACCESS_KEY_ID: 'AK',
  R2_SECRET_ACCESS_KEY: 'SK',
})

describe('app host gate', () => {
  it('serves /api/me with the dev identity and limits', async () => {
    const r = await call(app('/api/me'))
    expect(r.status).toBe(200)
    const body = (await r.json()) as any
    expect(body.identity).toBe('tester@example.com')
    expect(body.kind).toBe('user')
    expect(body.limits.max_upload_bytes).toBeGreaterThan(0)
  })

  it('401s every app-host path without credentials (api, server functions, pages)', async () => {
    for (const path of ['/api/me', '/_serverFn/abc', '/', '/assets/app.js']) {
      const r = await call(new Request(`http://localhost${path}`))
      expect(r.status, path).toBe(401)
      const body = (await r.json()) as any
      expect(body.error.code).toBe('unauthorized')
    }
  })

  it('fails closed in production: dev header is not an identity', async () => {
    const r = await call(
      new Request('https://cloud.example.com/api/me', { headers: { 'x-dev-identity': 'x@example.com' } }),
      { env: prodEnv },
    )
    expect(r.status).toBe(401)
  })

  it('accepts a valid Access JWT in production mode', async () => {
    const k = await makeAccessKeys()
    const jwt = await k.sign({ common_name: 'svc.access' })
    const r = await call(
      new Request('https://cloud.example.com/api/me', { headers: { 'cf-access-jwt-assertion': jwt } }),
      { env: prodEnv, jwks: k.jwks },
    )
    expect(r.status).toBe(200)
    expect(((await r.json()) as any).kind).toBe('service')
  })

  it('returns 500 (not open access) when Access config is missing', async () => {
    const r = await call(app('/api/me'), { env: envWith({ ACCESS_AUD: '' }) })
    expect(r.status).toBe(500)
  })

  it('403s a cross-origin POST before doing anything', async () => {
    const r = await call(
      app('/api/uploads', { ...json({}), headers: { 'content-type': 'application/json', origin: 'https://evil.example', 'x-dev-identity': 'a@b.c' } }),
    )
    expect(r.status).toBe(403)
    const server = await call(
      app('/_serverFn/x', { method: 'POST', headers: { origin: 'https://evil.example', 'x-dev-identity': 'a@b.c' } }),
    )
    expect(server.status).toBe(403)
    const bogus = await call(
      app('/api/uploads', { method: 'DELETE', headers: { origin: 'null', 'x-dev-identity': 'a@b.c' } }),
    )
    expect(bogus.status).toBe(403)
  })

  it('allows a same-origin POST and a POST without Origin', async () => {
    const same = await call(
      app('/api/uploads/' + '0'.repeat(26), { method: 'DELETE', headers: { origin: 'http://localhost:3000', 'x-dev-identity': 'a@b.c' } }),
    )
    expect(same.status).toBe(404) // got past the gate to the route
    const none = await call(app('/api/uploads/' + '0'.repeat(26), { method: 'DELETE' }))
    expect(none.status).toBe(404)
  })

  it('production only trusts https://APP_HOST as Origin', async () => {
    const k = await makeAccessKeys()
    const jwt = await k.sign({ common_name: 'svc' })
    const mk = (origin?: string) =>
      call(
        new Request('https://cloud.example.com/api/uploads/' + '0'.repeat(26), {
          method: 'DELETE',
          headers: { 'cf-access-jwt-assertion': jwt, ...(origin ? { origin } : {}) },
        }),
        { env: prodEnv, jwks: k.jwks },
      )
    expect((await mk('http://cloud.example.com')).status).toBe(403)
    expect((await mk('https://cloud.example.com')).status).toBe(404)
    expect((await mk()).status).toBe(404)
  })

  it('requires application/json for API bodies', async () => {
    const r = await call(
      app('/api/uploads', { method: 'POST', headers: { 'content-type': 'text/plain', 'x-dev-identity': 'a@b.c' }, body: '{}' }),
    )
    expect(r.status).toBe(415)
  })

  it('routes pages to Start and files to assets, both after auth, with explorer headers', async () => {
    const page = await call(app('/'))
    expect(page.status).toBe(200)
    expect(await page.text()).toContain(START_MARKER)
    expect(page.headers.get('content-security-policy')).toContain("object-src 'none'")
    const asset = await call(app('/assets/app.js'))
    expect(await asset.text()).toContain('asset')
    // non-GET server function calls reach Start
    const fn = await call(app('/_serverFn/abc', { method: 'POST' }), { next: fakeStart })
    expect(fn.status).toBe(200)
  })

  it('unknown /api paths use the error envelope', async () => {
    const r = await call(app('/api/nope'))
    expect(r.status).toBe(404)
    expect(((await r.json()) as any).error.code).toBe('not_found')
  })
})

describe('share host and unknown hosts', () => {
  const share = (path: string, init?: RequestInit) => call(new Request(`http://127.0.0.1${path}`, init))

  it('404s /api/*, /_serverFn/* and / on the share host, never reaching Start or assets', async () => {
    let startCalls = 0
    const next = () => {
      startCalls++
      return new Response('start')
    }
    for (const path of ['/api/me', '/api/items', '/_serverFn/abc', '/', '/assets/app.js', '/s', '/s/']) {
      const r = await call(new Request(`http://127.0.0.1${path}`, { headers: { 'x-dev-identity': 'a@b.c' } }), { next })
      expect(r.status, path).toBe(404)
    }
    const post = await share('/api/uploads', { method: 'POST' })
    expect(post.status).toBe(404)
    expect(startCalls).toBe(0)
  })

  it('404s an unknown host', async () => {
    const r = await call(new Request('http://evil.example/api/me', { headers: { 'x-dev-identity': 'a@b.c' } }))
    expect(r.status).toBe(404)
  })

  it('compares the URL hostname, not the Host header', async () => {
    const r = await call(new Request('http://evil.example/api/me', { headers: { host: 'localhost', 'x-dev-identity': 'a@b.c' } }))
    expect(r.status).toBe(404)
  })

  it('does not serve /__local in production', async () => {
    const r = await call(new Request('https://cloud.example.com/__local/put/x?exp=9999999999&sig=a', { method: 'PUT' }), {
      env: prodEnv,
    })
    expect(r.status).toBe(401) // falls through to the Access gate
    expect(baseEnv.ENVIRONMENT).toBe('test')
  })
})
