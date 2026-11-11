/**
 * Hostname dispatch and the gate in front of everything (plans/cloud.md section 5.3).
 * Framework-free: the TanStack Start handler is injected so tests need no Start build.
 */
import { createApi } from './api'
import { authenticate, type AuthDeps } from './auth/access'
import { isSafeMethod, originAllowed } from './auth/origin'
import { buildServices, type RequestContext } from './context'
import { ConfigError, loadConfig, type AppEnv, type Config } from './env'
import { errorResponse } from './errors'
import { runCleanup } from './cleanup'
import { handlePublic } from './public'
import { handleLocalStorage } from './storage/local'

export type NextHandler = (request: Request) => Promise<Response> | Response

export interface DispatchDeps extends AuthDeps {
  /** The TanStack Start handler: SSR pages and /_serverFn/*. */
  next: NextHandler
}

const EXPLORER_CSP =
  "default-src 'self'; img-src 'self' blob: data:; media-src 'self'; style-src 'self' 'unsafe-inline'; " +
  // Start injects inline hydration scripts and this installed version has no SSR nonce wiring.
  "script-src 'self' 'unsafe-inline'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'"

const api = createApi()

function notFound(): Response {
  return new Response('not found', {
    status: 404,
    headers: { 'content-type': 'text/plain; charset=utf-8', 'cache-control': 'no-store' },
  })
}

function misconfigured(e: unknown): Response {
  // Names of missing variables only, never values.
  console.error(e instanceof ConfigError ? e.message : 'configuration error')
  return new Response('server misconfigured', { status: 500, headers: { 'cache-control': 'no-store' } })
}

function withExplorerHeaders(response: Response): Response {
  const out = new Response(response.body, response)
  const type = out.headers.get('content-type') ?? ''
  if (type.includes('text/html')) {
    out.headers.set('content-security-policy', EXPLORER_CSP)
    out.headers.set('referrer-policy', 'same-origin')
    out.headers.set('cache-control', 'private, no-store')
  }
  out.headers.set('x-content-type-options', 'nosniff')
  return out
}

export async function dispatch(
  request: Request,
  env: AppEnv,
  exec: ExecutionContext,
  deps: DispatchDeps,
): Promise<Response> {
  let config: Config
  try {
    config = loadConfig(env)
  } catch (e) {
    return misconfigured(e)
  }
  const url = new URL(request.url)
  const host = url.hostname.toLowerCase()

  // Dev/test only: the local signer's upload endpoint. Authenticated by its HMAC signature.
  if (config.environment !== 'production' && url.pathname.startsWith('/__local/')) {
    return handleLocalStorage(request, env.BUCKET, { signingKey: env.LOCAL_SIGNING_KEY })
  }

  if (host === config.shareHost) {
    // The public page uses only this static font; all other assets stay private.
    if (isSafeMethod(request.method) && url.pathname === '/fonts/Geist.ttf') return env.ASSETS.fetch(request)
    return handlePublic(request, buildServices(env, url, config), exec)
  }

  if (host === config.appHost) {
    if (!isSafeMethod(request.method) && !originAllowed(request, config)) {
      return errorResponse('forbidden', 'cross-origin request refused')
    }
    const identity = await authenticate(request, config, deps)
    if (!identity) return errorResponse('unauthorized', 'authentication required')

    if (url.pathname === '/api' || url.pathname.startsWith('/api/')) {
      let services
      try {
        services = buildServices(env, url, config)
      } catch (e) {
        return misconfigured(e)
      }
      const rc: RequestContext = { services, identity }
      return api.fetch(request, { rc }, exec)
    }

    if (isSafeMethod(request.method)) {
      const asset = await env.ASSETS.fetch(request)
      if (asset.status !== 404) return withExplorerHeaders(asset)
    }
    return withExplorerHeaders(await deps.next(request))
  }

  return notFound()
}

/** Cron body. */
export async function runScheduled(env: AppEnv): Promise<void> {
  const config = loadConfig(env)
  const services = buildServices(env, new URL(`https://${config.appHost}/`), config)
  const result = await runCleanup(services)
  console.log('cleanup', JSON.stringify(result))
}
