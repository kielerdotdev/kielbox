/** All SQL for items. Prepared statements with bound values only. */

export type ItemStatus = 'pending' | 'ready'
export const ITEM_KINDS = ['screenshot', 'recording', 'replay', 'clip', 'other'] as const
export type ItemKind = (typeof ITEM_KINDS)[number]

export interface ItemRow {
  id: string
  status: ItemStatus
  kind: ItemKind
  title: string
  filename: string
  content_type: string
  size: number
  object_key: string
  thumb_key: string | null
  width: number | null
  height: number | null
  duration_ms: number | null
  captured_at: number
  created_at: number
  created_by: string
  favorite: number
  upload_id: string | null
  part_size: number | null
}

export type NewItem = Omit<ItemRow, 'thumb_key' | 'favorite' | 'status'>

export async function insertPending(db: D1Database, item: NewItem): Promise<void> {
  await db
    .prepare(
      `INSERT INTO items (id, status, kind, title, filename, content_type, size, object_key,
         width, height, duration_ms, captured_at, created_at, created_by, upload_id, part_size)
       VALUES (?, 'pending', ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
    )
    .bind(
      item.id,
      item.kind,
      item.title,
      item.filename,
      item.content_type,
      item.size,
      item.object_key,
      item.width,
      item.height,
      item.duration_ms,
      item.captured_at,
      item.created_at,
      item.created_by,
      item.upload_id,
      item.part_size,
    )
    .run()
}

export async function setUploadId(db: D1Database, id: string, uploadId: string, partSize: number): Promise<void> {
  await db.prepare(`UPDATE items SET upload_id = ?, part_size = ? WHERE id = ?`).bind(uploadId, partSize, id).run()
}

export async function getItem(db: D1Database, id: string): Promise<ItemRow | null> {
  return await db.prepare(`SELECT * FROM items WHERE id = ?`).bind(id).first<ItemRow>()
}

export async function markReady(db: D1Database, id: string, thumbKey: string | null): Promise<boolean> {
  const r = await db
    .prepare(`UPDATE items SET status = 'ready', upload_id = NULL, thumb_key = ? WHERE id = ? AND status = 'pending'`)
    .bind(thumbKey, id)
    .run()
  return (r.meta.changes ?? 0) > 0
}

export interface ListParams {
  limit: number
  cursor: { capturedAt: number; id: string } | null
  q: string | null
  kind: ItemKind | null
  favorite: boolean
}

/** Escape LIKE wildcards so user input matches literally (paired with ESCAPE '\'). */
export function escapeLike(input: string): string {
  return input.replace(/[\\%_]/g, (c) => `\\${c}`)
}

export async function listReady(db: D1Database, p: ListParams): Promise<ItemRow[]> {
  const where: string[] = [`status = 'ready'`]
  const binds: (string | number)[] = []
  if (p.cursor) {
    where.push(`(captured_at < ? OR (captured_at = ? AND id < ?))`)
    binds.push(p.cursor.capturedAt, p.cursor.capturedAt, p.cursor.id)
  }
  if (p.q) {
    where.push(`(title LIKE '%' || ? || '%' ESCAPE '\\' OR filename LIKE '%' || ? || '%' ESCAPE '\\')`)
    const escaped = escapeLike(p.q)
    binds.push(escaped, escaped)
  }
  if (p.kind) {
    where.push(`kind = ?`)
    binds.push(p.kind)
  }
  if (p.favorite) where.push(`favorite = 1`)
  binds.push(p.limit)
  const sql = `SELECT * FROM items WHERE ${where.join(' AND ')} ORDER BY captured_at DESC, id DESC LIMIT ?`
  const r = await db
    .prepare(sql)
    .bind(...binds)
    .all<ItemRow>()
  return r.results
}

export async function updateItem(
  db: D1Database,
  id: string,
  patch: { title?: string; favorite?: boolean },
): Promise<boolean> {
  const sets: string[] = []
  const binds: (string | number)[] = []
  if (patch.title !== undefined) {
    sets.push('title = ?')
    binds.push(patch.title)
  }
  if (patch.favorite !== undefined) {
    sets.push('favorite = ?')
    binds.push(patch.favorite ? 1 : 0)
  }
  if (sets.length === 0) return true
  binds.push(id)
  const r = await db
    .prepare(`UPDATE items SET ${sets.join(', ')} WHERE id = ? AND status = 'ready'`)
    .bind(...binds)
    .run()
  return (r.meta.changes ?? 0) > 0
}

/** Delete shares and the item in one batch so behaviour never depends on FK enforcement. */
export async function deleteItemAndShares(db: D1Database, id: string): Promise<void> {
  await db.batch([
    db.prepare(`DELETE FROM shares WHERE item_id = ?`).bind(id),
    db.prepare(`DELETE FROM items WHERE id = ?`).bind(id),
  ])
}

export async function listPendingOlderThan(db: D1Database, cutoffMs: number, limit: number): Promise<ItemRow[]> {
  const r = await db
    .prepare(`SELECT * FROM items WHERE status = 'pending' AND created_at < ? ORDER BY created_at LIMIT ?`)
    .bind(cutoffMs, limit)
    .all<ItemRow>()
  return r.results
}
