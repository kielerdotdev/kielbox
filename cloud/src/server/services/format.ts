import type { ItemRow } from '../db/items'
import type { ShareRow } from '../db/shares'

/** Public item JSON: never object_key, thumb_key or upload_id. */
export function itemJson(row: ItemRow) {
  return {
    id: row.id,
    status: row.status,
    kind: row.kind,
    title: row.title,
    filename: row.filename,
    content_type: row.content_type,
    size: row.size,
    width: row.width,
    height: row.height,
    duration_ms: row.duration_ms,
    captured_at: row.captured_at,
    created_at: row.created_at,
    created_by: row.created_by,
    favorite: row.favorite === 1,
    media_url: `/api/items/${row.id}/media`,
    thumb_url: row.thumb_key ? `/api/items/${row.id}/thumb` : null,
  }
}
export type ItemJson = ReturnType<typeof itemJson>

/** Share JSON: never the token hash. */
export function shareJson(row: ShareRow) {
  return {
    id: row.id,
    item_id: row.item_id,
    created_at: row.created_at,
    created_by: row.created_by,
    expires_at: row.expires_at,
    revoked_at: row.revoked_at,
    allow_download: row.allow_download === 1,
    view_count: row.view_count,
    last_viewed_at: row.last_viewed_at,
  }
}
export type ShareJson = ReturnType<typeof shareJson>
