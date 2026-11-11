import { env as rawEnv } from 'cloudflare:workers'
import { describe, expect, it } from 'vitest'
import { runCleanup } from '../src/server/cleanup'
import { buildServices } from '../src/server/context'
import { MIB, type AppEnv } from '../src/server/env'
import { app, baseEnv, bytes, call, json, putLocal, uploadItem } from './helpers'

const bucket = (rawEnv as unknown as AppEnv).BUCKET
const db = (rawEnv as unknown as AppEnv).DB

async function rangeGet(id: string, range?: string) {
  return call(app(`/api/items/${id}/media`, range ? { headers: { range, 'x-dev-identity': 'a@b.c' } } : {}))
}

describe('single upload flow', () => {
  it('create -> PUT -> complete -> list -> ranged reads', async () => {
    const data = bytes(1000)
    const item = await uploadItem({ data, title: 'Hello', filename: 'a b.mp4' })
    expect(item.status).toBe('ready')
    expect(item).not.toHaveProperty('object_key')
    expect(item).not.toHaveProperty('upload_id')
    expect(item).not.toHaveProperty('thumb_key')
    expect(item.media_url).toBe(`/api/items/${item.id}/media`)

    const list = (await (await call(app('/api/items'))).json()) as any
    expect(list.items.map((i: any) => i.id)).toEqual([item.id])
    expect(list.next_cursor).toBeNull()

    const full = await rangeGet(item.id)
    expect(full.status).toBe(200)
    expect(full.headers.get('accept-ranges')).toBe('bytes')
    expect(full.headers.get('etag')).toBeTruthy()
    expect(full.headers.get('x-content-type-options')).toBe('nosniff')
    expect(full.headers.get('content-security-policy')).toBe("default-src 'none'; sandbox")
    expect(full.headers.get('content-type')).toBe('video/mp4')
    expect(full.headers.get('content-disposition')).toMatch(/^inline; filename="a b.mp4"; filename\*=UTF-8''a%20b\.mp4$/)
    expect(new Uint8Array(await full.arrayBuffer())).toEqual(data)

    const part = await rangeGet(item.id, 'bytes=10-19')
    expect(part.status).toBe(206)
    expect(part.headers.get('content-range')).toBe('bytes 10-19/1000')
    expect(new Uint8Array(await part.arrayBuffer())).toEqual(data.slice(10, 20))

    const open = await rangeGet(item.id, 'bytes=990-')
    expect(open.status).toBe(206)
    expect(new Uint8Array(await open.arrayBuffer())).toEqual(data.slice(990))

    const suffix = await rangeGet(item.id, 'bytes=-5')
    expect(suffix.status).toBe(206)
    expect(suffix.headers.get('content-range')).toBe('bytes 995-999/1000')
    expect(new Uint8Array(await suffix.arrayBuffer())).toEqual(data.slice(995))

    const bad = await rangeGet(item.id, 'bytes=5000-6000')
    expect(bad.status).toBe(416)
    expect(bad.headers.get('content-range')).toBe('bytes */1000')

    const multi = await rangeGet(item.id, 'bytes=0-1,5-6')
    expect(multi.status).toBe(200)
    expect((await multi.arrayBuffer()).byteLength).toBe(1000)

    const dl = await rangeGet(item.id).then(() => call(app(`/api/items/${item.id}/media?download=1`)))
    expect(dl.headers.get('content-disposition')).toMatch(/^attachment;/)
  })

  it('records the thumbnail when uploaded and serves it as JPEG', async () => {
    const thumb = bytes(200, 3)
    const item = await uploadItem({ thumbnail: thumb })
    expect(item.thumb_url).toBe(`/api/items/${item.id}/thumb`)
    const r = await call(app(item.thumb_url))
    expect(r.status).toBe(200)
    expect(r.headers.get('content-type')).toBe('image/jpeg')
    expect(r.headers.get('cache-control')).toBe('private, max-age=86400')
    expect(new Uint8Array(await r.arrayBuffer())).toEqual(thumb)
  })

  it('drops an oversized thumbnail (> 1 MiB) on complete', async () => {
    const item = await uploadItem({ thumbnail: bytes(MIB + 1) })
    expect(item.thumb_url).toBeNull()
    const keys = (await bucket.list({ prefix: `u/${item.id}/` })).objects.map((o) => o.key)
    expect(keys).toEqual([`u/${item.id}/original.mp4`])
  })

  it('complete is idempotent on a ready item', async () => {
    const item = await uploadItem()
    const again = await call(app(`/api/uploads/${item.id}/complete`, { method: 'POST' }))
    expect(again.status).toBe(200)
    expect(((await again.json()) as any).item.id).toBe(item.id)
  })

  it('complete before the PUT is 409 upload_incomplete', async () => {
    const create = (await (
      await call(app('/api/uploads', json({ filename: 'a.png', size: 10, content_type: 'image/png', kind: 'screenshot', captured_at: 1 })))
    ).json()) as any
    const r = await call(app(`/api/uploads/${create.item_id}/complete`, { method: 'POST' }))
    expect(r.status).toBe(409)
    expect(((await r.json()) as any).error.code).toBe('upload_incomplete')
  })

  it('size mismatch is rejected and cleaned up', async () => {
    const create = (await (
      await call(app('/api/uploads', json({ filename: 'a.mp4', size: 100, content_type: 'video/mp4', kind: 'clip', captured_at: 1 })))
    ).json()) as any
    await putLocal(create.upload.url, create.upload.headers, bytes(50))
    const r = await call(app(`/api/uploads/${create.item_id}/complete`, { method: 'POST' }))
    expect(r.status).toBe(409)
    expect(await db.prepare('SELECT COUNT(*) AS n FROM items').first<{ n: number }>()).toEqual({ n: 0 })
    expect((await bucket.list()).objects).toHaveLength(0)
  })

  it('rejects disallowed types, oversize, bad filenames and malformed bodies', async () => {
    const mk = (over: Record<string, unknown>) =>
      call(app('/api/uploads', json({ filename: 'a.mp4', size: 10, content_type: 'video/mp4', kind: 'clip', captured_at: 1, ...over })))
    const type = await mk({ content_type: 'text/html' })
    expect(type.status).toBe(415)
    expect(((await type.json()) as any).error.code).toBe('unsupported_type')
    const big = await mk({ size: Number(baseEnv.MAX_UPLOAD_BYTES ?? 20 * 1024 ** 3) + 1 })
    expect(big.status).toBe(413)
    expect(((await big.json()) as any).error.code).toBe('too_large')
    expect((await mk({ size: 0 })).status).toBe(400)
    expect((await mk({ filename: 'a\u0000.mp4' })).status).toBe(400)
    expect((await mk({ filename: 'x'.repeat(256) })).status).toBe(400)
    expect((await mk({ kind: 'nope' })).status).toBe(400)
    const notJson = await call(app('/api/uploads', { method: 'POST', headers: { 'content-type': 'application/json', 'x-dev-identity': 'a@b.c' }, body: '{' }))
    expect(notJson.status).toBe(400)
    expect(await db.prepare('SELECT COUNT(*) AS n FROM items').first<{ n: number }>()).toEqual({ n: 0 })
  })

  it('object keys come from the content type, never the filename', async () => {
    const create = (await (
      await call(app('/api/uploads', json({ filename: '../../evil.php', size: 5, content_type: 'image/png', kind: 'screenshot', captured_at: 1 })))
    ).json()) as any
    const row = await db.prepare('SELECT object_key, title FROM items WHERE id = ?').bind(create.item_id).first<any>()
    expect(row.object_key).toBe(`u/${create.item_id}/original.png`)
    expect(row.title).toBe('../../evil')
  })

  it('refuses a PUT with a different content-type than signed', async () => {
    const create = (await (
      await call(app('/api/uploads', json({ filename: 'a.mp4', size: 5, content_type: 'video/mp4', kind: 'clip', captured_at: 1 })))
    ).json()) as any
    const r = await call(new Request(create.upload.url, { method: 'PUT', headers: { 'content-type': 'text/html' }, body: bytes(5) as BodyInit }))
    expect(r.status).toBe(403)
    const tampered = create.upload.url.replace('video%2Fmp4', 'text%2Fhtml')
    const r2 = await call(new Request(tampered, { method: 'PUT', headers: { 'content-type': 'text/html' }, body: bytes(5) as BodyInit }))
    expect(r2.status).toBe(403)
  })
})

describe('multipart', () => {
  it('uploads 3 parts, completes with ETags and reads back across a part boundary', async () => {
    const partSize = 5 * MIB
    const data = bytes(2 * partSize + 1234)
    const create = await call(
      app('/api/uploads', json({ filename: 'big.mkv', size: data.length, content_type: 'video/x-matroska', kind: 'recording', captured_at: 5 })),
    )
    expect(create.status).toBe(201)
    const body = (await create.json()) as any
    expect(body.upload).toEqual({ mode: 'multipart', part_size: partSize, part_count: 3 })

    const parts = (await (await call(app(`/api/uploads/${body.item_id}/parts`, json({ part_numbers: [1, 2, 3] })))).json()) as any
    expect(parts.parts).toHaveLength(3)

    // Completing too early fails cleanly.
    const early = await call(app(`/api/uploads/${body.item_id}/complete`, json({ parts: [] })))
    expect(early.status).toBe(400)

    const etags: { part_number: number; etag: string }[] = []
    for (const p of parts.parts) {
      const slice = data.slice((p.part_number - 1) * partSize, p.part_number * partSize)
      const r = await call(new Request(p.url, { method: 'PUT', body: slice as BodyInit }))
      expect(r.status).toBe(200)
      etags.push({ part_number: p.part_number, etag: r.headers.get('etag')! })
    }
    const done = await call(app(`/api/uploads/${body.item_id}/complete`, json({ parts: etags })))
    expect(done.status).toBe(200)
    const item = ((await done.json()) as any).item
    expect(item.size).toBe(data.length)

    const edge = await rangeGet(item.id, `bytes=${partSize - 4}-${partSize + 3}`)
    expect(edge.status).toBe(206)
    expect(new Uint8Array(await edge.arrayBuffer())).toEqual(data.slice(partSize - 4, partSize + 4))
    const row = await db.prepare('SELECT upload_id FROM items WHERE id = ?').bind(item.id).first<any>()
    expect(row.upload_id).toBeNull()
  })

  it('rejects part numbers out of range and aborting cleans everything', async () => {
    const create = (await (
      await call(app('/api/uploads', json({ filename: 'big.mp4', size: 13 * MIB, content_type: 'video/mp4', kind: 'clip', captured_at: 5 })))
    ).json()) as any
    expect(create.upload.mode).toBe('multipart')
    const bad = await call(app(`/api/uploads/${create.item_id}/parts`, json({ part_numbers: [99] })))
    expect(bad.status).toBe(400)
    const tooMany = await call(app(`/api/uploads/${create.item_id}/parts`, json({ part_numbers: Array.from({ length: 33 }, (_, i) => i + 1) })))
    expect(tooMany.status).toBe(400)
    const del = await call(app(`/api/uploads/${create.item_id}`, { method: 'DELETE' }))
    expect(del.status).toBe(204)
    expect(await db.prepare('SELECT COUNT(*) AS n FROM items').first<{ n: number }>()).toEqual({ n: 0 })
  })
})

describe('listing', () => {
  it('pages every item exactly once, newest first, including equal timestamps', async () => {
    const ids: string[] = []
    for (let i = 0; i < 7; i++) {
      const item = await uploadItem({ title: `item ${i}`, captured_at: 1_000 + Math.floor(i / 2) * 10 })
      ids.push(item.id)
    }
    const seen: string[] = []
    let cursor: string | null = null
    let pages = 0
    do {
      const url: string = `/api/items?limit=3${cursor ? `&cursor=${cursor}` : ''}`
      const page = (await (await call(app(url))).json()) as any
      seen.push(...page.items.map((i: any) => i.id))
      cursor = page.next_cursor
      pages++
    } while (cursor)
    expect(pages).toBe(3)
    expect(new Set(seen).size).toBe(7)
    expect([...seen].sort()).toEqual([...ids].sort())
    const times = (await Promise.all(seen.map((id) => db.prepare('SELECT captured_at FROM items WHERE id=?').bind(id).first<any>()))).map((r) => r.captured_at)
    expect(times).toEqual([...times].sort((a, b) => b - a))
  })

  it('treats LIKE wildcards in q literally', async () => {
    await uploadItem({ title: '100% done' })
    await uploadItem({ title: 'under_score' })
    await uploadItem({ title: 'plain text' })
    await uploadItem({ title: 'back\\slash' })
    const q = async (s: string) =>
      ((await (await call(app(`/api/items?q=${encodeURIComponent(s)}`))).json()) as any).items.map((i: any) => i.title)
    expect(await q('%')).toEqual(['100% done'])
    expect(await q('_')).toEqual(['under_score'])
    expect(await q('\\')).toEqual(['back\\slash'])
    expect(await q('0% d')).toEqual(['100% done'])
    expect(await q('t_xt')).toEqual([])
    expect(await q("' OR 1=1 --")).toEqual([])
    expect((await q('PLAIN'))).toEqual(['plain text']) // LIKE is case-insensitive for ASCII
  })

  it('filters by kind and favorite, and validates parameters', async () => {
    const a = await uploadItem({ kind: 'screenshot', content_type: 'image/png', filename: 's.png', title: 'shot' })
    await uploadItem({ kind: 'recording', title: 'rec' })
    const byKind = (await (await call(app('/api/items?kind=screenshot'))).json()) as any
    expect(byKind.items.map((i: any) => i.id)).toEqual([a.id])
    const patch = await call(app(`/api/items/${a.id}`, json({ favorite: true, title: 'renamed' }, 'PATCH')))
    expect(patch.status).toBe(200)
    expect(((await patch.json()) as any).item).toMatchObject({ favorite: true, title: 'renamed' })
    const favs = (await (await call(app('/api/items?favorite=1'))).json()) as any
    expect(favs.items.map((i: any) => i.id)).toEqual([a.id])
    expect((await call(app('/api/items?limit=0'))).status).toBe(400)
    expect((await call(app('/api/items?limit=101'))).status).toBe(400)
    expect((await call(app('/api/items?cursor=garbage!'))).status).toBe(400)
    expect((await call(app(`/api/items/${a.id}`, json({}, 'PATCH')))).status).toBe(400)
  })

  it('hides pending uploads from list, get, media', async () => {
    const create = (await (
      await call(app('/api/uploads', json({ filename: 'a.mp4', size: 5, content_type: 'video/mp4', kind: 'clip', captured_at: 1 })))
    ).json()) as any
    expect(((await (await call(app('/api/items'))).json()) as any).items).toHaveLength(0)
    expect((await call(app(`/api/items/${create.item_id}`))).status).toBe(404)
    expect((await call(app(`/api/items/${create.item_id}/media`))).status).toBe(404)
  })
})

describe('delete and cleanup', () => {
  it('deleting an item removes both objects and the row', async () => {
    const item = await uploadItem({ thumbnail: bytes(50) })
    expect((await bucket.list({ prefix: `u/${item.id}/` })).objects).toHaveLength(2)
    const r = await call(app(`/api/items/${item.id}`, { method: 'DELETE' }))
    expect(r.status).toBe(204)
    expect((await bucket.list()).objects).toHaveLength(0)
    expect((await call(app(`/api/items/${item.id}`))).status).toBe(404)
  })

  it('cron removes stale pending rows only', async () => {
    const stale = (await (
      await call(app('/api/uploads', json({ filename: 'a.mp4', size: 5, content_type: 'video/mp4', kind: 'clip', captured_at: 1, thumbnail: true })))
    ).json()) as any
    await putLocal(stale.upload.url, stale.upload.headers, bytes(5)) // object exists but never completed
    const fresh = (await (
      await call(app('/api/uploads', json({ filename: 'b.mp4', size: 5, content_type: 'video/mp4', kind: 'clip', captured_at: 1 })))
    ).json()) as any
    const ready = await uploadItem()
    await db.prepare('UPDATE items SET created_at = ? WHERE id = ?').bind(Date.now() - 25 * 3600_000, stale.item_id).run()
    await db.prepare('UPDATE items SET created_at = ? WHERE id = ?').bind(Date.now() - 48 * 3600_000, ready.id).run()

    const svc = buildServices(baseEnv, new URL('http://localhost/'))
    expect(await runCleanup(svc)).toEqual({ removed: 1, failed: 0 })
    const rows = (await db.prepare('SELECT id FROM items ORDER BY id').all<{ id: string }>()).results.map((r) => r.id)
    expect(rows.sort()).toEqual([fresh.item_id, ready.id].sort())
    expect((await bucket.list({ prefix: `u/${stale.item_id}/` })).objects).toHaveLength(0)
    expect((await bucket.list({ prefix: `u/${ready.id}/` })).objects).toHaveLength(1)
  })

  it('the scheduled export runs the cleanup', async () => {
    const mod = await import('../src/server/dispatch')
    const stale = (await (
      await call(app('/api/uploads', json({ filename: 'a.mp4', size: 5, content_type: 'video/mp4', kind: 'clip', captured_at: 1 })))
    ).json()) as any
    await db.prepare('UPDATE items SET created_at = 1 WHERE id = ?').bind(stale.item_id).run()
    await mod.runScheduled(baseEnv)
    expect(await db.prepare('SELECT COUNT(*) AS n FROM items').first<{ n: number }>()).toEqual({ n: 0 })
  })
})
