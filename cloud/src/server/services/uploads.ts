import type { Identity } from '../auth/access'
import type { Services } from '../context'
import { extensionFor } from '../content_types'
import * as items from '../db/items'
import { HttpError } from '../errors'
import { MIB } from '../env'
import { ulid } from '../ids'
import type { CreateUploadInput } from '../schemas'
import { StorageError } from '../storage/signer'
import { itemJson } from './format'

const SINGLE_TTL = 15 * 60
const PART_TTL = 60 * 60
const THUMB_TTL = 15 * 60
export const MAX_THUMB_BYTES = MIB
const MAX_PARTS = 9000

export function partSizeFor(svc: Services, size: number): number {
  const wanted = Math.max(svc.config.partSizeMin, Math.ceil(size / MAX_PARTS))
  return Math.ceil(wanted / MIB) * MIB
}

export function limits(svc: Services) {
  return {
    max_upload_bytes: svc.config.maxUploadBytes,
    multipart_threshold: svc.config.multipartThreshold,
    part_size: svc.config.partSizeMin,
  }
}

function stem(filename: string): string {
  const dot = filename.lastIndexOf('.')
  const s = dot > 0 ? filename.slice(0, dot) : filename
  return s.trim() || filename
}

const thumbKeyFor = (id: string) => `u/${id}/thumb.jpg`

export async function createUpload(svc: Services, who: Identity, input: CreateUploadInput) {
  if (input.size > svc.config.maxUploadBytes) {
    throw new HttpError('too_large', `size exceeds the ${svc.config.maxUploadBytes} byte limit`)
  }
  const ext = extensionFor(input.content_type)
  if (!ext) throw new HttpError('unsupported_type', 'content type is not supported')

  const now = svc.now()
  const id = ulid(now)
  const objectKey = `u/${id}/original.${ext}`
  const multipart = input.size >= svc.config.multipartThreshold
  const partSize = multipart ? partSizeFor(svc, input.size) : null

  await items.insertPending(svc.db, {
    id,
    kind: input.kind,
    title: input.title ?? stem(input.filename),
    filename: input.filename,
    content_type: input.content_type,
    size: input.size,
    object_key: objectKey,
    width: input.width ?? null,
    height: input.height ?? null,
    duration_ms: input.duration_ms ?? null,
    captured_at: input.captured_at,
    created_at: now,
    created_by: who.identity,
    upload_id: null,
    part_size: partSize,
  })

  try {
    let upload: Record<string, unknown>
    if (multipart && partSize) {
      const uploadId = await svc.signer.createMultipart(objectKey, input.content_type)
      await items.setUploadId(svc.db, id, uploadId, partSize)
      upload = { mode: 'multipart', part_size: partSize, part_count: Math.ceil(input.size / partSize) }
    } else {
      const url = await svc.signer.presignPut(objectKey, input.content_type, SINGLE_TTL)
      upload = {
        mode: 'single',
        url,
        headers: { 'content-type': input.content_type },
        expires_at: now + SINGLE_TTL * 1000,
      }
    }
    const body: Record<string, unknown> = { item_id: id, upload }
    if (input.thumbnail) {
      const url = await svc.signer.presignPut(thumbKeyFor(id), 'image/jpeg', THUMB_TTL)
      body.thumbnail_upload = {
        mode: 'single',
        url,
        headers: { 'content-type': 'image/jpeg' },
        expires_at: now + THUMB_TTL * 1000,
      }
    }
    return body
  } catch (e) {
    // Presigning failed: do not leave a pending row behind.
    await items.deleteItemAndShares(svc.db, id).catch(() => {})
    if (e instanceof HttpError) throw e
    console.error('create upload failed', id, e instanceof StorageError ? e.code : 'error')
    throw new HttpError('internal', 'could not start the upload')
  }
}

async function requirePending(svc: Services, id: string): Promise<items.ItemRow> {
  const row = await items.getItem(svc.db, id)
  if (!row) throw new HttpError('not_found', 'upload not found')
  return row
}

export async function signParts(svc: Services, id: string, partNumbers: number[]) {
  const row = await requirePending(svc, id)
  if (row.status !== 'pending' || !row.upload_id || !row.part_size) {
    throw new HttpError('conflict', 'this upload is not an open multipart upload')
  }
  const partCount = Math.ceil(row.size / row.part_size)
  const uniq = [...new Set(partNumbers)]
  if (uniq.some((n) => n < 1 || n > partCount)) {
    throw new HttpError('invalid_request', `part numbers must be between 1 and ${partCount}`)
  }
  const now = svc.now()
  const parts = await Promise.all(
    uniq.map(async (n) => ({
      part_number: n,
      url: await svc.signer.presignPart(row.object_key, row.upload_id!, n, PART_TTL),
      expires_at: now + PART_TTL * 1000,
    })),
  )
  return { parts }
}

async function discard(svc: Services, row: items.ItemRow): Promise<void> {
  const keys = [row.object_key, row.thumb_key ?? thumbKeyFor(row.id)]
  await svc.bucket.delete(keys)
  await items.deleteItemAndShares(svc.db, row.id)
}

export async function completeUpload(
  svc: Services,
  id: string,
  parts: { part_number: number; etag: string }[] | undefined,
) {
  const row = await requirePending(svc, id)
  if (row.status === 'ready') return { item: itemJson(row) }

  if (row.upload_id && row.part_size) {
    const partCount = Math.ceil(row.size / row.part_size)
    const sorted = [...(parts ?? [])].sort((a, b) => a.part_number - b.part_number)
    const contiguous =
      sorted.length === partCount && sorted.every((p, i) => p.part_number === i + 1)
    if (!contiguous) {
      throw new HttpError('invalid_request', `expected parts 1..${partCount} with their etags`)
    }
    try {
      await svc.signer.completeMultipart(
        row.object_key,
        row.upload_id,
        sorted.map((p) => ({ partNumber: p.part_number, etag: p.etag })),
      )
    } catch (e) {
      // A retried request after a dropped response finds the upload already completed:
      // the head() check below decides.
      if (!(await svc.bucket.head(row.object_key))) {
        console.error('complete multipart failed', id, e instanceof StorageError ? e.code : 'error')
        throw new HttpError('upload_incomplete', 'the storage service rejected the uploaded parts')
      }
    }
  } else if (parts && parts.length > 0) {
    throw new HttpError('invalid_request', 'parts are only valid for multipart uploads')
  }

  const head = await svc.bucket.head(row.object_key)
  if (!head) throw new HttpError('upload_incomplete', 'the object has not been uploaded')
  if (head.size !== row.size) {
    await discard(svc, row)
    throw new HttpError('conflict', 'uploaded size does not match the declared size; the upload was discarded')
  }

  let thumbKey: string | null = null
  const thumb = await svc.bucket.head(thumbKeyFor(id))
  if (thumb) {
    if (thumb.size <= MAX_THUMB_BYTES) thumbKey = thumbKeyFor(id)
    else await svc.bucket.delete(thumbKeyFor(id))
  }

  await items.markReady(svc.db, id, thumbKey)
  const fresh = await items.getItem(svc.db, id)
  return { item: itemJson(fresh!) }
}

/** Abort a pending upload; ready items are removed through the item delete route instead. */
export async function abortUpload(svc: Services, id: string): Promise<void> {
  const row = await requirePending(svc, id)
  if (row.status !== 'pending') throw new HttpError('conflict', 'upload already completed; delete the item instead')
  if (row.upload_id) await svc.signer.abortMultipart(row.object_key, row.upload_id).catch(() => {})
  await discard(svc, row)
}
