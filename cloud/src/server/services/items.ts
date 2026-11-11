import type { Services } from '../context'
import * as db from '../db/items'
import { HttpError } from '../errors'
import { ULID_RE, base64urlDecode, base64urlEncode } from '../ids'
import { itemJson, shareJson } from './format'
import * as shareDb from '../db/shares'

export interface ListInput {
  limit?: number
  cursor?: string
  q?: string
  kind?: db.ItemKind
  favorite?: boolean
}

export function encodeCursor(capturedAt: number, id: string): string {
  return base64urlEncode(new TextEncoder().encode(`${capturedAt}:${id}`))
}

export function decodeCursor(cursor: string): { capturedAt: number; id: string } | null {
  const bytes = base64urlDecode(cursor)
  if (!bytes) return null
  const m = /^(\d{1,17}):([0-9A-HJKMNP-TV-Z]{26})$/.exec(new TextDecoder().decode(bytes))
  return m ? { capturedAt: Number(m[1]), id: m[2]! } : null
}

export async function listItems(svc: Services, input: ListInput) {
  const limit = Math.min(100, Math.max(1, input.limit ?? 50))
  let cursor: { capturedAt: number; id: string } | null = null
  if (input.cursor) {
    cursor = decodeCursor(input.cursor)
    if (!cursor) throw new HttpError('invalid_request', 'invalid cursor')
  }
  const rows = await db.listReady(svc.db, {
    limit: limit + 1,
    cursor,
    q: input.q?.trim() ? input.q.trim() : null,
    kind: input.kind ?? null,
    favorite: input.favorite === true,
  })
  const page = rows.slice(0, limit)
  const last = page[page.length - 1]
  return {
    items: page.map(itemJson),
    next_cursor: rows.length > limit && last ? encodeCursor(last.captured_at, last.id) : null,
  }
}

/** A ready item, or 404 (pending items are invisible outside the upload routes). */
export async function requireReady(svc: Services, id: string): Promise<db.ItemRow> {
  if (!ULID_RE.test(id)) throw new HttpError('not_found', 'item not found')
  const row = await db.getItem(svc.db, id)
  if (!row || row.status !== 'ready') throw new HttpError('not_found', 'item not found')
  return row
}

export async function getItemWithShares(svc: Services, id: string) {
  const row = await requireReady(svc, id)
  const shares = await shareDb.listSharesForItem(svc.db, id)
  return { item: itemJson(row), shares: shares.map(shareJson) }
}

export async function patchItem(svc: Services, id: string, patch: { title?: string; favorite?: boolean }) {
  await requireReady(svc, id)
  await db.updateItem(svc.db, id, patch)
  return { item: itemJson(await requireReady(svc, id)) }
}

export async function deleteItem(svc: Services, id: string): Promise<void> {
  const row = await requireReady(svc, id)
  // Objects first: if this fails the row survives and the delete can be retried.
  await svc.bucket.delete([row.object_key, row.thumb_key ?? `u/${row.id}/thumb.jpg`])
  await db.deleteItemAndShares(svc.db, id)
}
