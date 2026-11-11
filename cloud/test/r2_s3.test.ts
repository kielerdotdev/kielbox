import { describe, expect, it } from 'vitest'
import { R2S3Signer } from '../src/server/storage/r2_s3'
import { LocalSigner } from '../src/server/storage/local'
import { env } from 'cloudflare:workers'
import type { AppEnv } from '../src/server/env'

const fixed = () => new Date('2026-01-02T03:04:05Z')
const mk = (fetchImpl?: (r: Request) => Promise<Response>) =>
  new R2S3Signer({ accountId: 'acct123', bucket: 'kielbox-cloud', accessKeyId: 'AKIDEXAMPLE', secretAccessKey: 'secret', now: fixed, fetchImpl })

describe('R2S3Signer presigning', () => {
  it('signs a PUT for the R2 endpoint with expiry and a signed content-type', async () => {
    const url = new URL(await mk().presignPut('u/ABC/original.mp4', 'video/mp4', 900))
    expect(url.host).toBe('acct123.r2.cloudflarestorage.com')
    expect(url.pathname).toBe('/kielbox-cloud/u/ABC/original.mp4')
    expect(url.searchParams.get('X-Amz-Expires')).toBe('900')
    expect(url.searchParams.get('X-Amz-Algorithm')).toBe('AWS4-HMAC-SHA256')
    expect(url.searchParams.get('X-Amz-Date')).toBe('20260102T030405Z')
    expect(url.searchParams.get('X-Amz-Credential')).toBe('AKIDEXAMPLE/20260102/auto/s3/aws4_request')
    expect(url.searchParams.get('X-Amz-SignedHeaders')?.split(';')).toContain('content-type')
    expect(url.searchParams.get('X-Amz-Signature')).toMatch(/^[0-9a-f]{64}$/)
    expect(url.search).not.toContain('secret')
  })

  it('is deterministic for a fixed clock and sensitive to the content type', async () => {
    const a = await mk().presignPut('u/ABC/original.mp4', 'video/mp4', 900)
    const b = await mk().presignPut('u/ABC/original.mp4', 'video/mp4', 900)
    const c = await mk().presignPut('u/ABC/original.mp4', 'video/webm', 900)
    expect(a).toBe(b)
    expect(a).not.toBe(c)
  })

  it('presigns a part with partNumber and uploadId', async () => {
    const url = new URL(await mk().presignPart('u/ABC/original.mp4', 'UP1', 3, 3600))
    expect(url.searchParams.get('partNumber')).toBe('3')
    expect(url.searchParams.get('uploadId')).toBe('UP1')
    expect(url.searchParams.get('X-Amz-Expires')).toBe('3600')
  })
})

describe('R2S3Signer control plane', () => {
  it('creates, completes and aborts multipart uploads through the S3 API', async () => {
    const seen: { method: string; url: string; body: string; auth: boolean }[] = []
    const signer = mk(async (r) => {
      seen.push({ method: r.method, url: r.url, body: await r.clone().text(), auth: (r.headers.get('authorization') ?? '').startsWith('AWS4-HMAC-SHA256') })
      if (r.method === 'POST' && r.url.includes('?uploads')) return new Response('<InitiateMultipartUploadResult><UploadId>UP-9</UploadId></InitiateMultipartUploadResult>')
      return new Response('<CompleteMultipartUploadResult/>')
    })
    expect(await signer.createMultipart('u/A/original.mp4', 'video/mp4')).toBe('UP-9')
    await signer.completeMultipart('u/A/original.mp4', 'UP-9', [{ partNumber: 1, etag: '"abc"' }])
    await signer.abortMultipart('u/A/original.mp4', 'UP-9')
    expect(seen.map((s) => s.method)).toEqual(['POST', 'POST', 'DELETE'])
    expect(seen.every((s) => s.auth)).toBe(true)
    expect(seen[0]!.url).toContain('uploads=')
    expect(seen[1]!.url).toContain('uploadId=UP-9')
    expect(seen[1]!.body).toContain('<PartNumber>1</PartNumber><ETag>&quot;abc&quot;</ETag>')
  })

  it('surfaces an S3 error code, including a 200 response carrying <Error>', async () => {
    const signer = mk(async () => new Response('<Error><Code>InvalidPart</Code></Error>', { status: 200 }))
    await expect(signer.completeMultipart('k', 'u', [{ partNumber: 1, etag: 'x' }])).rejects.toMatchObject({ code: 'InvalidPart' })
    const failing = mk(async () => new Response('<Error><Code>NoSuchUpload</Code></Error>', { status: 404 }))
    await expect(failing.abortMultipart('k', 'u')).rejects.toMatchObject({ code: 'NoSuchUpload' })
  })
})

describe('LocalSigner', () => {
  it('refuses to be constructed in production', () => {
    const e = env as unknown as AppEnv
    expect(() => new LocalSigner(e.BUCKET, { environment: 'production', signingKey: 'k', origin: 'http://x' })).toThrow()
  })
})
