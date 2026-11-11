import type { Identity } from '../auth/access'
import type { Services } from '../context'
import * as shareDb from '../db/shares'
import { HttpError } from '../errors'
import { ULID_RE, decodeShareToken, newShareToken, sha256Hex, ulid } from '../ids'
import { shareJson } from './format'
import { requireReady } from './items'

export const DEFAULT_SHARE_TTL_SECONDS = 7 * 86_400

export async function createShare(
  svc: Services,
  who: Identity,
  itemId: string,
  input: { expires_in_seconds?: number | null; allow_download?: boolean },
) {
  await requireReady(svc, itemId)
  const now = svc.now()
  const ttl = input.expires_in_seconds === undefined ? DEFAULT_SHARE_TTL_SECONDS : input.expires_in_seconds
  const token = newShareToken()
  const id = ulid(now)
  await shareDb.insertShare(svc.db, {
    id,
    item_id: itemId,
    token_hash: await sha256Hex(decodeShareToken(token)!),
    created_at: now,
    created_by: who.identity,
    expires_at: ttl === null ? null : now + ttl * 1000,
    allow_download: input.allow_download === false ? 0 : 1,
  })
  const row = (await shareDb.getShare(svc.db, id))!
  // The only time the raw token exists outside the link itself.
  return { share: shareJson(row), url: `${svc.shareOrigin}/s/${token}` }
}

export async function listShares(svc: Services, itemId: string) {
  await requireReady(svc, itemId)
  return { shares: (await shareDb.listSharesForItem(svc.db, itemId)).map(shareJson) }
}

export async function revokeShare(svc: Services, shareId: string): Promise<void> {
  if (!ULID_RE.test(shareId)) throw new HttpError('not_found', 'share not found')
  const row = await shareDb.getShare(svc.db, shareId)
  if (!row) throw new HttpError('not_found', 'share not found')
  await shareDb.revokeShare(svc.db, shareId, svc.now())
}

/**
 * Resolve a presented token to a live share. A malformed token is rejected before D1 is
 * touched. Unknown, revoked and expired tokens are indistinguishable (all null).
 */
export async function resolveShare(svc: Services, token: string) {
  const bytes = decodeShareToken(token)
  if (!bytes) return null
  const found = await shareDb.findByTokenHash(svc.db, await sha256Hex(bytes))
  if (!found) return null
  if (found.revoked_at !== null) return null
  if (found.expires_at !== null && found.expires_at <= svc.now()) return null
  return found
}
