import { Hono, type Context } from 'hono'
import { ZodError, type ZodType } from 'zod'
import type { RequestContext } from './context'
import { HttpError, errorResponse } from './errors'
import { streamObject } from './media'
import { ULID_RE } from './ids'
import {
  completeSchema,
  createShareSchema,
  createUploadSchema,
  listQuerySchema,
  partsRequestSchema,
  patchItemSchema,
} from './schemas'
import * as itemsSvc from './services/items'
import * as sharesSvc from './services/shares'
import * as uploads from './services/uploads'

type Bindings = { rc: RequestContext }
type Ctx = Context<{ Bindings: Bindings }>

const MAX_BODY_BYTES = 64 * 1024
const JSON_TYPE = /^application\/json\s*(;|$)/i

function hasBody(request: Request): boolean {
  if (request.body === null) return false
  return request.headers.get('content-length') !== '0'
}

/** Parse and validate a JSON body. An absent body counts as `{}` only when `optional`. */
async function body<T>(c: Ctx, schema: ZodType<T>, optional = false): Promise<T> {
  const req = c.req.raw
  let raw: unknown = {}
  if (hasBody(req)) {
    const text = await req.text()
    if (new TextEncoder().encode(text).length > MAX_BODY_BYTES) {
      throw new HttpError('too_large', 'request body is too large')
    }
    try {
      raw = text === '' ? {} : JSON.parse(text)
    } catch {
      throw new HttpError('invalid_request', 'body is not valid JSON')
    }
  } else if (!optional) {
    throw new HttpError('invalid_request', 'a JSON body is required')
  }
  return schema.parse(raw)
}

function itemId(c: Ctx): string {
  const id = c.req.param('id')
  if (!id || !ULID_RE.test(id)) throw new HttpError('not_found', 'not found')
  return id
}

/** The desktop's JSON API under /api/*. Auth and CSRF have already run in the dispatcher. */
export function createApi() {
  const api = new Hono<{ Bindings: Bindings }>().basePath('/api')
  const svc = (c: Ctx) => c.env.rc.services
  const who = (c: Ctx) => c.env.rc.identity

  api.use('*', async (c, next) => {
    const m = c.req.method
    if ((m === 'POST' || m === 'PATCH' || m === 'PUT' || m === 'DELETE') && hasBody(c.req.raw)) {
      if (!JSON_TYPE.test(c.req.header('content-type') ?? '')) {
        throw new HttpError('unsupported_type', 'Content-Type must be application/json')
      }
    }
    await next()
    // Responses of the authenticated API are never cacheable by shared caches.
    if (!c.res.headers.has('cache-control')) c.res.headers.set('cache-control', 'private, no-store')
    c.res.headers.set('x-content-type-options', 'nosniff')
  })

  api.onError((err) => {
    if (err instanceof HttpError) return errorResponse(err.code, err.message, err.status)
    if (err instanceof ZodError) {
      const first = err.issues[0]
      const where = first?.path.length ? `${first.path.join('.')}: ` : ''
      return errorResponse('invalid_request', `${where}${first?.message ?? 'invalid request'}`)
    }
    // Never echo internals; log only the error name.
    console.error('unhandled api error', err instanceof Error ? err.name : typeof err)
    return errorResponse('internal', 'internal error')
  })
  api.notFound(() => errorResponse('not_found', 'not found'))

  api.get('/me', (c) => {
    const { identity, kind } = who(c)
    return c.json({ identity, kind, limits: uploads.limits(svc(c)) })
  })

  api.post('/uploads', async (c) => {
    const input = await body(c, createUploadSchema)
    return c.json(await uploads.createUpload(svc(c), who(c), input), 201)
  })
  api.post('/uploads/:id/parts', async (c) => {
    const input = await body(c, partsRequestSchema)
    return c.json(await uploads.signParts(svc(c), itemId(c), input.part_numbers))
  })
  api.post('/uploads/:id/complete', async (c) => {
    const input = await body(c, completeSchema, true)
    return c.json(await uploads.completeUpload(svc(c), itemId(c), input.parts))
  })
  api.delete('/uploads/:id', async (c) => {
    await uploads.abortUpload(svc(c), itemId(c))
    return c.body(null, 204)
  })

  api.get('/items', async (c) => {
    const q = listQuerySchema.parse(Object.fromEntries(new URL(c.req.url).searchParams))
    return c.json(
      await itemsSvc.listItems(svc(c), {
        limit: q.limit,
        cursor: q.cursor,
        q: q.q,
        kind: q.kind,
        favorite: q.favorite !== undefined,
      }),
    )
  })
  api.get('/items/:id', async (c) => c.json(await itemsSvc.getItemWithShares(svc(c), itemId(c))))
  api.patch('/items/:id', async (c) => {
    const patch = await body(c, patchItemSchema)
    return c.json(await itemsSvc.patchItem(svc(c), itemId(c), patch))
  })
  api.delete('/items/:id', async (c) => {
    await itemsSvc.deleteItem(svc(c), itemId(c))
    return c.body(null, 204)
  })

  api.on(['GET', 'HEAD'], '/items/:id/media', async (c) => {
    const row = await itemsSvc.requireReady(svc(c), itemId(c))
    return streamObject(svc(c).bucket, c.req.raw, {
      key: row.object_key,
      contentType: row.content_type,
      filename: row.filename,
      download: new URL(c.req.url).searchParams.get('download') === '1',
      cacheControl: 'private, no-cache',
    })
  })
  api.on(['GET', 'HEAD'], '/items/:id/thumb', async (c) => {
    const row = await itemsSvc.requireReady(svc(c), itemId(c))
    if (!row.thumb_key) throw new HttpError('not_found', 'no thumbnail')
    return streamObject(svc(c).bucket, c.req.raw, {
      key: row.thumb_key,
      contentType: 'image/jpeg',
      filename: 'thumbnail.jpg',
      download: false,
      cacheControl: 'private, max-age=86400',
    })
  })

  api.post('/items/:id/shares', async (c) => {
    const input = await body(c, createShareSchema, true)
    return c.json(await sharesSvc.createShare(svc(c), who(c), itemId(c), input), 201)
  })
  api.get('/items/:id/shares', async (c) => c.json(await sharesSvc.listShares(svc(c), itemId(c))))
  api.delete('/shares/:id', async (c) => {
    const id = c.req.param('id')
    await sharesSvc.revokeShare(svc(c), id)
    return c.body(null, 204)
  })

  return api
}
