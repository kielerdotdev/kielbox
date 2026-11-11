/** Worker bindings, variables and validated configuration. Framework-free. */

export type Environment = 'production' | 'dev' | 'test'

export interface AppEnv {
  BUCKET: R2Bucket
  DB: D1Database
  ASSETS: Fetcher
  APP_HOST: string
  SHARE_HOST: string
  ACCESS_TEAM_DOMAIN: string
  ACCESS_AUD: string
  R2_ACCOUNT_ID: string
  R2_BUCKET_NAME: string
  MAX_UPLOAD_BYTES?: string
  ENVIRONMENT: string
  /** Secrets (production). */
  R2_ACCESS_KEY_ID?: string
  R2_SECRET_ACCESS_KEY?: string
  /** Dev/test only. */
  LOCAL_SIGNING_KEY?: string
  /** Optional overrides of the multipart sizing (R2 needs parts of at least 5 MiB). */
  MULTIPART_THRESHOLD_BYTES?: string
  PART_SIZE_MIN_BYTES?: string
}

export const MIB = 1024 * 1024
export const GIB = 1024 * MIB

export class ConfigError extends Error {
  constructor(message: string) {
    super(message)
    this.name = 'ConfigError'
  }
}

export interface Config {
  environment: Environment
  appHost: string
  shareHost: string
  accessTeamDomain: string
  accessAud: string
  r2AccountId: string
  r2BucketName: string
  maxUploadBytes: number
  multipartThreshold: number
  partSizeMin: number
}

function required(env: AppEnv, name: keyof AppEnv, missing: string[]): string {
  const value = env[name]
  if (typeof value !== 'string' || value.trim() === '') {
    missing.push(name)
    return ''
  }
  return value.trim()
}

function positiveInt(raw: string | undefined, fallback: number, name: string, min = 1): number {
  if (raw === undefined || raw === '') return fallback
  const n = Number(raw)
  if (!Number.isSafeInteger(n) || n < min) throw new ConfigError(`${name} must be an integer >= ${min}`)
  return n
}

/**
 * Validate the environment. Throws a ConfigError naming the missing variables (never their
 * values). There is deliberately no "auth disabled" default.
 */
export function loadConfig(env: AppEnv): Config {
  const missing: string[] = []
  const environment = required(env, 'ENVIRONMENT', missing)
  const appHost = required(env, 'APP_HOST', missing).toLowerCase()
  const shareHost = required(env, 'SHARE_HOST', missing).toLowerCase()
  const accessTeamDomain = required(env, 'ACCESS_TEAM_DOMAIN', missing).replace(/\/+$/, '')
  const accessAud = required(env, 'ACCESS_AUD', missing)
  const r2AccountId = required(env, 'R2_ACCOUNT_ID', missing)
  const r2BucketName = required(env, 'R2_BUCKET_NAME', missing)
  if (missing.length > 0) throw new ConfigError(`missing configuration: ${missing.join(', ')}`)
  if (environment !== 'production' && environment !== 'dev' && environment !== 'test') {
    throw new ConfigError('ENVIRONMENT must be production, dev or test')
  }
  if (appHost === shareHost) throw new ConfigError('APP_HOST and SHARE_HOST must differ')
  if (!/^https:\/\/[^/]+$/.test(accessTeamDomain) && environment === 'production') {
    throw new ConfigError('ACCESS_TEAM_DOMAIN must look like https://<team>.cloudflareaccess.com')
  }
  return {
    environment,
    appHost,
    shareHost,
    accessTeamDomain,
    accessAud,
    r2AccountId,
    r2BucketName,
    maxUploadBytes: positiveInt(env.MAX_UPLOAD_BYTES, 20 * GIB, 'MAX_UPLOAD_BYTES'),
    multipartThreshold: positiveInt(env.MULTIPART_THRESHOLD_BYTES, 64 * MIB, 'MULTIPART_THRESHOLD_BYTES', 5 * MIB),
    partSizeMin: positiveInt(env.PART_SIZE_MIN_BYTES, 32 * MIB, 'PART_SIZE_MIN_BYTES', 5 * MIB),
  }
}
