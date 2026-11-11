/** Range-capable streaming from R2 with the response headers from plans/cloud.md section 6. */

export type ParsedRange =
  | { kind: 'none' }
  | { kind: 'unsatisfiable' }
  | { kind: 'range'; offset: number; length: number }

/** Parse a single `bytes=a-b`, `bytes=a-` or `bytes=-n` range. Anything else falls back to a full 200. */
export function parseRange(header: string | null, size: number): ParsedRange {
  if (!header) return { kind: 'none' }
  const m = /^bytes=(\d*)-(\d*)$/i.exec(header.trim())
  if (!m) return { kind: 'none' } // malformed or multi-range
  const [, a, b] = m as unknown as [string, string, string]
  if (a === '' && b === '') return { kind: 'none' }
  if (a === '') {
    const n = Number(b)
    if (!Number.isSafeInteger(n) || n === 0) return { kind: 'unsatisfiable' }
    const length = Math.min(n, size)
    return { kind: 'range', offset: size - length, length }
  }
  const start = Number(a)
  if (!Number.isSafeInteger(start) || start >= size) return { kind: 'unsatisfiable' }
  if (b === '') return { kind: 'range', offset: start, length: size - start }
  const end = Number(b)
  if (!Number.isSafeInteger(end) || end < start) return { kind: 'none' } // invalid: ignore per RFC 9110
  return { kind: 'range', offset: start, length: Math.min(end, size - 1) - start + 1 }
}

/** `attachment; filename="ascii"; filename*=UTF-8''pct` (RFC 6266 / 5987). */
export function contentDisposition(filename: string, download: boolean): string {
  const ascii = filename.replace(/[^\x20-\x7e]/g, '_').replace(/["\\%]/g, '_')
  const encoded = encodeURIComponent(filename).replace(
    /['()*]/g,
    (c) => `%${c.charCodeAt(0).toString(16).toUpperCase()}`,
  )
  return `${download ? 'attachment' : 'inline'}; filename="${ascii}"; filename*=UTF-8''${encoded}`
}

export interface MediaOptions {
  key: string
  contentType: string
  filename: string
  download: boolean
  cacheControl: string
}

export async function streamObject(bucket: R2Bucket, request: Request, o: MediaOptions): Promise<Response> {
  const head = await bucket.head(o.key)
  if (!head) return new Response('not found', { status: 404, headers: { 'cache-control': 'no-store' } })

  const headers = new Headers({
    'content-type': o.contentType,
    'accept-ranges': 'bytes',
    etag: head.httpEtag,
    'x-content-type-options': 'nosniff',
    'content-security-policy': "default-src 'none'; sandbox",
    'content-disposition': contentDisposition(o.filename, o.download),
    'cache-control': o.cacheControl,
    'cross-origin-resource-policy': 'same-origin',
  })
  const size = head.size
  const range = parseRange(request.headers.get('range'), size)

  if (range.kind === 'unsatisfiable') {
    headers.set('content-range', `bytes */${size}`)
    return new Response(null, { status: 416, headers })
  }
  const isHead = request.method === 'HEAD'
  if (range.kind === 'range') {
    headers.set('content-range', `bytes ${range.offset}-${range.offset + range.length - 1}/${size}`)
    headers.set('content-length', String(range.length))
    if (isHead) return new Response(null, { status: 206, headers })
    const obj = await bucket.get(o.key, { range: { offset: range.offset, length: range.length } })
    if (!obj || !('body' in obj)) return new Response('not found', { status: 404 })
    return new Response(obj.body, { status: 206, headers })
  }
  headers.set('content-length', String(size))
  if (isHead) return new Response(null, { status: 200, headers })
  const obj = await bucket.get(o.key)
  if (!obj || !('body' in obj)) return new Response('not found', { status: 404 })
  return new Response(obj.body, { status: 200, headers })
}
