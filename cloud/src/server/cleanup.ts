import type { Services } from './context'
import * as items from './db/items'

const STALE_MS = 24 * 60 * 60 * 1000
const BATCH = 200

/** Cron body: drop pending uploads older than 24 h (multipart, objects, row). Ids-only logging. */
export async function runCleanup(svc: Services): Promise<{ removed: number; failed: number }> {
  const rows = await items.listPendingOlderThan(svc.db, svc.now() - STALE_MS, BATCH)
  let removed = 0
  let failed = 0
  for (const row of rows) {
    try {
      if (row.upload_id) await svc.signer.abortMultipart(row.object_key, row.upload_id).catch(() => {})
      await svc.bucket.delete([row.object_key, row.thumb_key ?? `u/${row.id}/thumb.jpg`])
      await items.deleteItemAndShares(svc.db, row.id)
      removed++
    } catch {
      failed++
      console.error('cleanup failed for', row.id)
    }
  }
  return { removed, failed }
}
