import { toHex } from '../ids'
import { StorageError, type UploadSigner } from './signer'

/**
 * Dev/test signer. Miniflare has no S3 endpoint, so presigned URLs point back at this Worker
 * (`/__local/...`, HMAC-SHA-256 over the request parameters) and the handler writes through
 * the R2 binding. Constructing it in production throws.
 */
export class LocalSigner implements UploadSigner {
  constructor(
    private readonly bucket: R2Bucket,
    private readonly opts: {
      environment: string
      signingKey: string | undefined
      /** Origin the client reaches this Worker on, e.g. http://localhost:3000. */
      origin: string
      now?: () => number
    },
  ) {
    if (opts.environment === 'production') throw new Error('LocalSigner is not available in production')
    if (!opts.signingKey) throw new Error('LOCAL_SIGNING_KEY is required for the local signer')
  }

  private now(): number {
    return (this.opts.now ?? Date.now)()
  }

  private signingKey(): string {
    return this.opts.signingKey!
  }

  async presignPut(key: string, contentType: string, ttlSeconds: number): Promise<string> {
    const exp = Math.floor(this.now() / 1000) + ttlSeconds
    const sig = await localSignature(this.signingKey(), ['put', key, String(exp), contentType])
    const url = new URL(`/__local/put/${encodeKey(key)}`, this.opts.origin)
    url.searchParams.set('exp', String(exp))
    url.searchParams.set('ct', contentType)
    url.searchParams.set('sig', sig)
    return url.toString()
  }

  async presignPart(key: string, uploadId: string, partNumber: number, ttlSeconds: number): Promise<string> {
    const exp = Math.floor(this.now() / 1000) + ttlSeconds
    const sig = await localSignature(this.signingKey(), ['part', key, uploadId, String(partNumber), String(exp)])
    const url = new URL(`/__local/part/${encodeKey(key)}`, this.opts.origin)
    url.searchParams.set('uploadId', uploadId)
    url.searchParams.set('partNumber', String(partNumber))
    url.searchParams.set('exp', String(exp))
    url.searchParams.set('sig', sig)
    return url.toString()
  }

  async createMultipart(key: string, contentType: string): Promise<string> {
    const upload = await this.bucket.createMultipartUpload(key, { httpMetadata: { contentType } })
    return upload.uploadId
  }

  async completeMultipart(
    key: string,
    uploadId: string,
    parts: { partNumber: number; etag: string }[],
  ): Promise<void> {
    try {
      await this.bucket
        .resumeMultipartUpload(key, uploadId)
        .complete(parts.map((p) => ({ partNumber: p.partNumber, etag: p.etag.replace(/^"|"$/g, '') })))
    } catch (e) {
      throw new StorageError('complete_failed', e instanceof Error ? e.message : 'complete failed')
    }
  }

  async abortMultipart(key: string, uploadId: string): Promise<void> {
    try {
      await this.bucket.resumeMultipartUpload(key, uploadId).abort()
    } catch (e) {
      throw new StorageError('abort_failed', e instanceof Error ? e.message : 'abort failed')
    }
  }
}

function encodeKey(key: string): string {
  return key.split('/').map(encodeURIComponent).join('/')
}

export async function localSignature(secret: string, fields: string[]): Promise<string> {
  const enc = new TextEncoder()
  const key = await crypto.subtle.importKey('raw', enc.encode(secret), { name: 'HMAC', hash: 'SHA-256' }, false, [
    'sign',
  ])
  return toHex(await crypto.subtle.sign('HMAC', key, enc.encode(fields.join('\n'))))
}

function timingSafeEqualHex(a: string, b: string): boolean {
  if (a.length !== b.length) return false
  let diff = 0
  for (let i = 0; i < a.length; i++) diff |= a.charCodeAt(i) ^ b.charCodeAt(i)
  return diff === 0
}

function plain(status: number, message: string): Response {
  return new Response(message, { status, headers: { 'content-type': 'text/plain; charset=utf-8' } })
}

/** Handler for `/__local/put/<key>` and `/__local/part/<key>`. Only mounted outside production. */
export async function handleLocalStorage(
  request: Request,
  bucket: R2Bucket,
  opts: { signingKey: string | undefined; now?: () => number },
): Promise<Response> {
  if (!opts.signingKey) return plain(404, 'not found')
  if (request.method !== 'PUT') return plain(405, 'method not allowed')
  const url = new URL(request.url)
  const m = /^\/__local\/(put|part)\/(.+)$/.exec(url.pathname)
  if (!m) return plain(404, 'not found')
  const op = m[1]!
  let key: string
  try {
    key = decodeURIComponent(m[2]!)
  } catch {
    return plain(400, 'bad key')
  }
  const exp = url.searchParams.get('exp') ?? ''
  const sig = url.searchParams.get('sig') ?? ''
  const nowS = Math.floor((opts.now ?? Date.now)() / 1000)
  if (!/^\d+$/.test(exp) || Number(exp) < nowS) return plain(403, 'expired')

  if (op === 'put') {
    // Mirror S3: content-type is part of the signature and the client cannot change it.
    const signedType = url.searchParams.get('ct') ?? ''
    const expected = await localSignature(opts.signingKey, ['put', key, exp, signedType])
    if (!timingSafeEqualHex(expected, sig)) return plain(403, 'bad signature')
    if ((request.headers.get('content-type') ?? '') !== signedType) return plain(403, 'content-type mismatch')
    const body = await bodyForR2(request)
    const object = await bucket.put(key, body, { httpMetadata: { contentType: signedType } })
    return new Response(null, { status: 200, headers: { etag: object.httpEtag } })
  }

  const uploadId = url.searchParams.get('uploadId') ?? ''
  const partNumber = url.searchParams.get('partNumber') ?? ''
  const expected = await localSignature(opts.signingKey, ['part', key, uploadId, partNumber, exp])
  if (!timingSafeEqualHex(expected, sig)) return plain(403, 'bad signature')
  try {
    const part = await bucket
      .resumeMultipartUpload(key, uploadId)
      .uploadPart(Number(partNumber), await bodyForR2(request))
    return new Response(null, { status: 200, headers: { etag: `"${part.etag}"` } })
  } catch {
    return plain(400, 'part upload failed')
  }
}

/** R2 needs a known-length body; buffer only when the client did not send Content-Length. */
async function bodyForR2(request: Request): Promise<ReadableStream | ArrayBuffer> {
  if (request.body === null) return new ArrayBuffer(0)
  if (request.headers.has('content-length')) return request.body
  return await request.arrayBuffer()
}
