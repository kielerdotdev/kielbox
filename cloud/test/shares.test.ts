import { env as rawEnv } from 'cloudflare:workers'
import { describe, expect, it, vi } from 'vitest'
import { sha256Hex, base64urlDecode } from '../src/server/ids'
import type { AppEnv } from '../src/server/env'
import { app, bytes, call, json, uploadItem } from './helpers'

const db = (rawEnv as unknown as AppEnv).DB

async function makeShare(itemId: string, body: unknown = {}) {
  const r = await call(app(`/api/items/${itemId}/shares`, json(body)))
  expect(r.status).toBe(201)
  return (await r.json()) as { share: any; url: string }
}
const shareReq = (url: string, suffix = '', init?: RequestInit) => call(new Request(url + suffix, init))

describe('creating shares', () => {
  it('returns the token once and stores only its SHA-256', async () => {
    const item = await uploadItem()
    const { share, url } = await makeShare(item.id)
    const token = url.split('/s/')[1]!
    expect(url).toBe(`http://127.0.0.1/s/${token}`)
    expect(token).toMatch(/^[A-Za-z0-9_-]{43}$/)

    // JSON for the share never carries the token or its hash.
    expect(JSON.stringify(share)).not.toContain(token)
    expect(share).not.toHaveProperty('token_hash')
    const listed = await (await call(app(`/api/items/${item.id}/shares`))).text()
    expect(listed).not.toContain(token)
    expect(listed).not.toContain('token_hash')
    const detail = await (await call(app(`/api/items/${item.id}`))).text()
    expect(detail).not.toContain(token)

    // D1 holds the hash of the raw bytes and not the token itself.
    const rows = (await db.prepare('SELECT * FROM shares').all<any>()).results
    expect(rows).toHaveLength(1)
    expect(rows[0].token_hash).toBe(await sha256Hex(base64urlDecode(token)!))
    expect(JSON.stringify(rows)).not.toContain(token)

    // Two shares for one item get different tokens.
    const second = await makeShare(item.id, { expires_in_seconds: null })
    expect(second.url).not.toBe(url)
    expect(second.share.expires_at).toBeNull()
  })

  it('defaults to 7 days, validates input, and only shares ready items', async () => {
    const item = await uploadItem()
    const { share } = await makeShare(item.id)
    expect(share.expires_at - share.created_at).toBe(7 * 86_400_000)
    expect((await call(app(`/api/items/${item.id}/shares`, json({ expires_in_seconds: -5 })))).status).toBe(400)
    expect((await call(app(`/api/items/${item.id}/shares`, json({ allow_download: 'yes' })))).status).toBe(400)
    const pending = (await (
      await call(app('/api/uploads', json({ filename: 'a.mp4', size: 5, content_type: 'video/mp4', kind: 'clip', captured_at: 1 })))
    ).json()) as any
    expect((await call(app(`/api/items/${pending.item_id}/shares`, json({})))).status).toBe(404)
    // body-less POST is allowed
    expect((await call(app(`/api/items/${item.id}/shares`, { method: 'POST' }))).status).toBe(201)
  })
})

describe('share host', () => {
  it('serves the page, media with ranges, and the thumbnail', async () => {
    const data = bytes(500)
    const thumb = bytes(80, 9)
    const item = await uploadItem({ data, title: 'My clip', thumbnail: thumb })
    const { url } = await makeShare(item.id)

    const page = await shareReq(url)
    expect(page.status).toBe(200)
    const html = await page.text()
    expect(html).toContain('<video controls')
    expect(html).toContain('My clip')
    expect(html).toContain('Download')
    expect(html).toContain('property="og:title"')
    expect(html).toContain('property="og:image"')
    expect(html).toContain('property="og:video"')
    expect(html).not.toMatch(/<script/i)

    const media = await shareReq(url, '/media', { headers: { range: 'bytes=100-199' } })
    expect(media.status).toBe(206)
    expect(media.headers.get('content-range')).toBe('bytes 100-199/500')
    expect(new Uint8Array(await media.arrayBuffer())).toEqual(data.slice(100, 200))
    expect((await shareReq(url, '/media', { headers: { range: 'bytes=900-' } })).status).toBe(416)

    const t = await shareReq(url, '/thumb')
    expect(t.status).toBe(200)
    expect(t.headers.get('content-type')).toBe('image/jpeg')
    expect(new Uint8Array(await t.arrayBuffer())).toEqual(thumb)

    const dl = await shareReq(url, '/media?download=1')
    expect(dl.status).toBe(200)
    expect(dl.headers.get('content-disposition')).toMatch(/^attachment;/)

    // Other paths and methods on a valid token do not exist.
    expect((await shareReq(url, '/other')).status).toBe(404)
    expect((await shareReq(url, '/', {})).status).toBe(404)
    expect((await shareReq(url, '', { method: 'POST' })).status).toBe(404)
  })

  it('sends every section 6.5 header on the page and the 404 page', async () => {
    const item = await uploadItem()
    const { url } = await makeShare(item.id)
    for (const r of [await shareReq(url), await shareReq('http://127.0.0.1/s/' + 'A'.repeat(43))]) {
      expect(r.headers.get('content-security-policy')).toBe(
        "default-src 'none'; img-src 'self'; media-src 'self'; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'",
      )
      expect(r.headers.get('referrer-policy')).toBe('no-referrer')
      expect(r.headers.get('x-robots-tag')).toBe('noindex, nofollow')
      expect(r.headers.get('cache-control')).toBe('private, no-store')
      expect(r.headers.get('content-type')).toContain('text/html')
    }
  })

  it('counts views on the page route only', async () => {
    const item = await uploadItem()
    const { share, url } = await makeShare(item.id)
    await shareReq(url)
    await shareReq(url)
    await shareReq(url, '/media')
    await shareReq(url, '/thumb')
    const row = await db.prepare('SELECT view_count, last_viewed_at FROM shares WHERE id = ?').bind(share.id).first<any>()
    expect(row.view_count).toBe(2)
    expect(row.last_viewed_at).toBeGreaterThan(0)
  })

  it('escapes the title and filename in the page', async () => {
    const evil = `<script>alert(1)</script>"'&`
    const item = await uploadItem({ title: evil, filename: `<img src=x onerror=alert(2)>.mp4` })
    const { url } = await makeShare(item.id)
    const html = await (await shareReq(url)).text()
    expect(html).not.toContain('<script>')
    expect(html).not.toContain('<img src=x')
    expect(html).toContain('&lt;script&gt;alert(1)&lt;/script&gt;&quot;&#39;&amp;')
    expect(html).toContain('&lt;img src=x onerror=alert(2)&gt;.mp4')
  })

  it('renders images with <img> and no video markup', async () => {
    const item = await uploadItem({ content_type: 'image/png', filename: 's.png', kind: 'screenshot', title: 'Shot' })
    const { url } = await makeShare(item.id)
    const html = await (await shareReq(url)).text()
    expect(html).toContain('<img src="/s/')
    expect(html).not.toContain('<video')
  })

  it('refuses download when allow_download is false but still streams inline', async () => {
    const item = await uploadItem()
    const { url, share } = await makeShare(item.id, { allow_download: false })
    expect(share.allow_download).toBe(false)
    const dl = await shareReq(url, '/media?download=1')
    expect(dl.status).toBe(403)
    expect((await shareReq(url, '/media')).status).toBe(200)
    const html = await (await shareReq(url)).text()
    expect(html).not.toContain('Download')
  })
})

describe('invalid links all look the same', () => {
  it('unknown, revoked and expired tokens return an identical 404', async () => {
    const item = await uploadItem()
    const live = await makeShare(item.id)
    const revoked = await makeShare(item.id)
    const expired = await makeShare(item.id)
    await db.prepare('UPDATE shares SET expires_at = ? WHERE id = ?').bind(Date.now() - 1000, expired.share.id).run()
    const rev = await call(app(`/api/shares/${revoked.share.id}`, { method: 'DELETE' }))
    expect(rev.status).toBe(204)
    expect((await call(app(`/api/shares/${revoked.share.id}`, { method: 'DELETE' }))).status).toBe(204) // idempotent

    const unknown = 'http://127.0.0.1/s/' + 'B'.repeat(42) + 'A'
    const bodies: string[] = []
    for (const u of [unknown, revoked.url, expired.url]) {
      for (const suffix of ['', '/media', '/thumb']) {
        const r = await shareReq(u, suffix)
        expect(r.status, u + suffix).toBe(404)
        if (suffix === '') bodies.push(await r.text())
      }
    }
    expect(new Set(bodies).size).toBe(1)
    expect((await shareReq(live.url)).status).toBe(200)
    expect((await call(app(`/api/shares/${'0'.repeat(26)}`, { method: 'DELETE' }))).status).toBe(404)
  })

  it('malformed tokens 404 without a D1 query', async () => {
    const item = await uploadItem()
    const live = await makeShare(item.id)
    const prepare = vi.spyOn((rawEnv as unknown as AppEnv).DB, 'prepare')
    const bad = [
      'short',
      'A'.repeat(42),
      'A'.repeat(44),
      'A'.repeat(42) + '=',
      'A'.repeat(42) + '!',
      'A'.repeat(42) + 'B', // non-canonical trailing bits
      '..%2F..%2Fetc',
      '%00' + 'A'.repeat(40),
    ]
    for (const t of bad) {
      for (const suffix of ['', '/media', '/thumb']) {
        const r = await call(new Request(`http://127.0.0.1/s/${t}${suffix}`))
        expect(r.status, t).toBe(404)
      }
    }
    expect(prepare).not.toHaveBeenCalled()
    // Sanity: the spy does see queries for a well-formed token.
    await shareReq(live.url, '/thumb')
    expect(prepare).toHaveBeenCalled()
    prepare.mockRestore()
  })
})

describe('lifecycle', () => {
  it('deleting an item kills its shares', async () => {
    const item = await uploadItem()
    const { url } = await makeShare(item.id)
    expect((await shareReq(url)).status).toBe(200)
    expect((await call(app(`/api/items/${item.id}`, { method: 'DELETE' }))).status).toBe(204)
    expect((await shareReq(url)).status).toBe(404)
    expect((await shareReq(url, '/media')).status).toBe(404)
    expect(await db.prepare('SELECT COUNT(*) AS n FROM shares').first<{ n: number }>()).toEqual({ n: 0 })
  })

  it('revoking takes effect immediately and shows in the share list', async () => {
    const item = await uploadItem()
    const { share, url } = await makeShare(item.id)
    expect((await shareReq(url)).status).toBe(200)
    await call(app(`/api/shares/${share.id}`, { method: 'DELETE' }))
    expect((await shareReq(url)).status).toBe(404)
    const list = (await (await call(app(`/api/items/${item.id}/shares`))).json()) as any
    expect(list.shares[0].revoked_at).toBeGreaterThan(0)
  })

  it('share links are not reachable on the app host', async () => {
    const item = await uploadItem()
    const { url } = await makeShare(item.id)
    const r = await call(app(new URL(url).pathname))
    expect(r.status).toBe(200) // falls to the (fake) Start handler, not the share page
    expect(await r.text()).not.toContain('<video')
  })
})
