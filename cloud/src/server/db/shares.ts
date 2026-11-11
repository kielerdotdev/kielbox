import type { ItemRow } from './items'

export interface ShareRow {
  id: string
  item_id: string
  token_hash: string
  created_at: number
  created_by: string
  expires_at: number | null
  revoked_at: number | null
  allow_download: number
  view_count: number
  last_viewed_at: number | null
}

export async function insertShare(db: D1Database, s: Omit<ShareRow, 'revoked_at' | 'view_count' | 'last_viewed_at'>) {
  await db
    .prepare(
      `INSERT INTO shares (id, item_id, token_hash, created_at, created_by, expires_at, allow_download)
       VALUES (?, ?, ?, ?, ?, ?, ?)`,
    )
    .bind(s.id, s.item_id, s.token_hash, s.created_at, s.created_by, s.expires_at, s.allow_download)
    .run()
}

export async function getShare(db: D1Database, id: string): Promise<ShareRow | null> {
  return await db.prepare(`SELECT * FROM shares WHERE id = ?`).bind(id).first<ShareRow>()
}

export async function listSharesForItem(db: D1Database, itemId: string): Promise<ShareRow[]> {
  const r = await db
    .prepare(`SELECT * FROM shares WHERE item_id = ? ORDER BY created_at DESC, id DESC`)
    .bind(itemId)
    .all<ShareRow>()
  return r.results
}

export async function revokeShare(db: D1Database, id: string, now: number): Promise<void> {
  await db.prepare(`UPDATE shares SET revoked_at = ? WHERE id = ? AND revoked_at IS NULL`).bind(now, id).run()
}

export type ShareWithItem = ShareRow & { item: ItemRow }

/** Look a share up by the SHA-256 of its token. Returns the ready item it points at. */
export async function findByTokenHash(db: D1Database, hash: string): Promise<ShareWithItem | null> {
  const row = await db
    .prepare(
      `SELECT s.id AS s_id, s.item_id AS s_item_id, s.token_hash AS s_token_hash, s.created_at AS s_created_at,
              s.created_by AS s_created_by, s.expires_at AS s_expires_at, s.revoked_at AS s_revoked_at,
              s.allow_download AS s_allow_download, s.view_count AS s_view_count, s.last_viewed_at AS s_last_viewed_at,
              i.*
         FROM shares s JOIN items i ON i.id = s.item_id
        WHERE s.token_hash = ? AND i.status = 'ready'`,
    )
    .bind(hash)
    .first<Record<string, unknown>>()
  if (!row) return null
  const share: ShareRow = {
    id: row.s_id as string,
    item_id: row.s_item_id as string,
    token_hash: row.s_token_hash as string,
    created_at: row.s_created_at as number,
    created_by: row.s_created_by as string,
    expires_at: row.s_expires_at as number | null,
    revoked_at: row.s_revoked_at as number | null,
    allow_download: row.s_allow_download as number,
    view_count: row.s_view_count as number,
    last_viewed_at: row.s_last_viewed_at as number | null,
  }
  const item = Object.fromEntries(Object.entries(row).filter(([k]) => !k.startsWith('s_'))) as unknown as ItemRow
  return { ...share, item }
}

export async function recordView(db: D1Database, id: string, now: number): Promise<void> {
  await db
    .prepare(`UPDATE shares SET view_count = view_count + 1, last_viewed_at = ? WHERE id = ?`)
    .bind(now, id)
    .run()
}
