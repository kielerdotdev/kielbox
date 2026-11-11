import { createRemoteJWKSet, decodeProtectedHeader, jwtVerify, type JWTVerifyGetKey } from 'jose'
import type { Config } from '../env'

export interface Identity {
  /** Access email, or the service token's common_name. */
  identity: string
  kind: 'user' | 'service'
}

export type JwksResolver = JWTVerifyGetKey

const remoteSets = new Map<string, JwksResolver>()

/** Remote JWKS for the team domain, cached per isolate (jose also caches the keys). */
export function remoteJwks(teamDomain: string): JwksResolver {
  let set = remoteSets.get(teamDomain)
  if (!set) {
    set = createRemoteJWKSet(new URL(`${teamDomain}/cdn-cgi/access/certs`))
    remoteSets.set(teamDomain, set)
  }
  return set
}

export const ACCESS_JWT_HEADER = 'Cf-Access-Jwt-Assertion'
export const DEV_IDENTITY_HEADER = 'X-Dev-Identity'

function isLoopbackHost(hostname: string): boolean {
  return hostname === 'localhost' || hostname === '127.0.0.1'
}

/** The dev bypass exists only outside production and only for loopback hostnames. */
export function devBypassAllowed(config: Config, request: Request): boolean {
  if (config.environment !== 'dev' && config.environment !== 'test') return false
  return isLoopbackHost(new URL(request.url).hostname)
}

export interface AuthDeps {
  jwks?: JwksResolver
}

/**
 * Authenticate a request. Returns the identity, or null when the request must be refused
 * with 401. Never throws for bad credentials; fails closed.
 */
export async function authenticate(
  request: Request,
  config: Config,
  deps: AuthDeps = {},
): Promise<Identity | null> {
  if (devBypassAllowed(config, request)) {
    const dev = request.headers.get(DEV_IDENTITY_HEADER)?.trim()
    if (dev && dev.length <= 256) {
      return { identity: dev, kind: dev.includes('@') ? 'user' : 'service' }
    }
  }
  const token = request.headers.get(ACCESS_JWT_HEADER)
  if (!token) return null
  // Cheap structural rejection of garbage before any key lookup.
  if (token.length > 8192 || !/^[\w-]+\.[\w-]+\.[\w-]+$/.test(token)) return null
  try {
    if (decodeProtectedHeader(token).alg !== 'RS256') return null
    const jwks = deps.jwks ?? remoteJwks(config.accessTeamDomain)
    const { payload } = await jwtVerify(token, jwks, {
      issuer: config.accessTeamDomain,
      audience: config.accessAud,
      algorithms: ['RS256'],
    })
    if (typeof payload.email === 'string' && payload.email !== '') {
      return { identity: payload.email, kind: 'user' }
    }
    if (typeof payload.common_name === 'string' && payload.common_name !== '') {
      return { identity: payload.common_name, kind: 'service' }
    }
    return null
  } catch {
    // Deliberately no logging of the token or the verification error detail.
    return null
  }
}
