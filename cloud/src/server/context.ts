import type { Identity } from './auth/access'
import { loadConfig, type AppEnv, type Config } from './env'
import { LocalSigner } from './storage/local'
import { R2S3Signer } from './storage/r2_s3'
import type { UploadSigner } from './storage/signer'

/** Everything a service needs; built once per request (or cron run). */
export interface Services {
  config: Config
  db: D1Database
  bucket: R2Bucket
  signer: UploadSigner
  /** Origin the share links are built on, e.g. https://share.example.com. */
  shareOrigin: string
  now: () => number
}

export interface RequestContext {
  services: Services
  identity: Identity
}

/** Production uses the R2 S3 signer; dev and test use the local signer. */
export function makeSigner(env: AppEnv, config: Config, origin: string): UploadSigner {
  if (config.environment === 'production') {
    if (!env.R2_ACCESS_KEY_ID || !env.R2_SECRET_ACCESS_KEY) {
      throw new Error('missing configuration: R2_ACCESS_KEY_ID, R2_SECRET_ACCESS_KEY')
    }
    return new R2S3Signer({
      accountId: config.r2AccountId,
      bucket: config.r2BucketName,
      accessKeyId: env.R2_ACCESS_KEY_ID,
      secretAccessKey: env.R2_SECRET_ACCESS_KEY,
    })
  }
  return new LocalSigner(env.BUCKET, {
    environment: config.environment,
    signingKey: env.LOCAL_SIGNING_KEY,
    origin,
  })
}

/** Where share links point. In dev the scheme and port follow the incoming request. */
export function shareOriginFor(config: Config, requestUrl: URL): string {
  if (config.environment === 'production') return `https://${config.shareHost}`
  const port = requestUrl.port ? `:${requestUrl.port}` : ''
  return `${requestUrl.protocol}//${config.shareHost}${port}`
}

export function buildServices(env: AppEnv, requestUrl: URL, config: Config = loadConfig(env)): Services {
  return {
    config,
    db: env.DB,
    bucket: env.BUCKET,
    signer: makeSigner(env, config, requestUrl.origin),
    shareOrigin: shareOriginFor(config, requestUrl),
    now: Date.now,
  }
}
