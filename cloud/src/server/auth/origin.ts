import type { Config } from '../env'

/**
 * CSRF check for browser mutations on the app host. A request with an Origin header is only
 * accepted from the app origin itself; requests without one (service-token clients) pass.
 * Outside production the dev server speaks plain http on an arbitrary port.
 */
export function originAllowed(request: Request, config: Config): boolean {
  const origin = request.headers.get('origin')
  if (origin === null) return true
  if (origin === `https://${config.appHost}`) return true
  if (config.environment !== 'production') {
    try {
      const u = new URL(origin)
      return u.protocol === 'http:' && u.hostname === config.appHost && u.origin === origin
    } catch {
      return false
    }
  }
  return false
}

export function isSafeMethod(method: string): boolean {
  return method === 'GET' || method === 'HEAD'
}
