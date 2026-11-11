import { AwsClient } from 'aws4fetch'
import { StorageError, type UploadSigner } from './signer'

export interface R2S3Options {
  accountId: string
  bucket: string
  accessKeyId: string
  secretAccessKey: string
  /** Injected for deterministic presigning in tests. */
  now?: () => Date
  /** Injected for the control-plane calls in tests. */
  fetchImpl?: (request: Request) => Promise<Response>
}

function amzDate(d: Date): string {
  return d.toISOString().replace(/[:-]|\.\d{3}/g, '')
}

function encodeKey(key: string): string {
  return key.split('/').map(encodeURIComponent).join('/')
}

function xmlEscape(text: string): string {
  return text.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;')
}

/**
 * Production signer: presigned URLs against the R2 S3 endpoint. Presigned URLs only work on
 * `<account>.r2.cloudflarestorage.com`, never on a custom domain.
 */
export class R2S3Signer implements UploadSigner {
  private readonly client: AwsClient
  private readonly base: string
  private readonly now: () => Date
  private readonly send: (request: Request) => Promise<Response>

  constructor(opts: R2S3Options) {
    this.client = new AwsClient({
      accessKeyId: opts.accessKeyId,
      secretAccessKey: opts.secretAccessKey,
      service: 's3',
      region: 'auto',
      retries: 2,
    })
    this.base = `https://${opts.accountId}.r2.cloudflarestorage.com/${encodeURIComponent(opts.bucket)}`
    this.now = opts.now ?? (() => new Date())
    this.send = opts.fetchImpl ?? ((r) => fetch(r))
  }

  private objectUrl(key: string, query: Record<string, string> = {}): URL {
    const url = new URL(`${this.base}/${encodeKey(key)}`)
    for (const [k, v] of Object.entries(query)) url.searchParams.set(k, v)
    return url
  }

  private async presign(
    method: string,
    url: URL,
    ttlSeconds: number,
    headers: Record<string, string> = {},
  ): Promise<string> {
    url.searchParams.set('X-Amz-Expires', String(Math.max(1, Math.floor(ttlSeconds))))
    const signed = await this.client.sign(url.toString(), {
      method,
      headers,
      // content-type is unsignable by default in aws4fetch; sign every header we set.
      aws: { signQuery: true, allHeaders: true, datetime: amzDate(this.now()) },
    })
    return signed.url
  }

  presignPut(key: string, contentType: string, ttlSeconds: number): Promise<string> {
    return this.presign('PUT', this.objectUrl(key), ttlSeconds, { 'content-type': contentType })
  }

  presignPart(key: string, uploadId: string, partNumber: number, ttlSeconds: number): Promise<string> {
    return this.presign(
      'PUT',
      this.objectUrl(key, { partNumber: String(partNumber), uploadId }),
      ttlSeconds,
    )
  }

  private async control(
    method: string,
    url: URL,
    opts: { body?: string; headers?: Record<string, string> } = {},
  ): Promise<string> {
    const signed = await this.client.sign(url.toString(), {
      method,
      headers: opts.headers,
      body: opts.body,
      aws: { datetime: amzDate(this.now()) },
    })
    const response = await this.send(signed)
    const text = await response.text()
    // R2 (like S3) can answer 200 with an <Error> body for CompleteMultipartUpload.
    const code = /<Code>([^<]*)<\/Code>/.exec(text)?.[1]
    if (!response.ok || (code && /<Error>/.test(text))) {
      throw new StorageError(code ?? `http_${response.status}`, `storage request failed (${code ?? response.status})`)
    }
    return text
  }

  async createMultipart(key: string, contentType: string): Promise<string> {
    const text = await this.control('POST', this.objectUrl(key, { uploads: '' }), {
      headers: { 'content-type': contentType },
    })
    const id = /<UploadId>([^<]+)<\/UploadId>/.exec(text)?.[1]
    if (!id) throw new StorageError('no_upload_id', 'storage did not return an upload id')
    return id
  }

  async completeMultipart(
    key: string,
    uploadId: string,
    parts: { partNumber: number; etag: string }[],
  ): Promise<void> {
    const body =
      '<CompleteMultipartUpload>' +
      parts
        .map((p) => `<Part><PartNumber>${p.partNumber}</PartNumber><ETag>${xmlEscape(p.etag)}</ETag></Part>`)
        .join('') +
      '</CompleteMultipartUpload>'
    await this.control('POST', this.objectUrl(key, { uploadId }), {
      body,
      headers: { 'content-type': 'application/xml' },
    })
  }

  async abortMultipart(key: string, uploadId: string): Promise<void> {
    await this.control('DELETE', this.objectUrl(key, { uploadId }))
  }
}
