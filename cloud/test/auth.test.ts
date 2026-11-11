import { describe, expect, it } from 'vitest'
import { authenticate } from '../src/server/auth/access'
import { loadConfig } from '../src/server/env'
import { baseEnv, envWith, makeAccessKeys } from './helpers'

const prodEnv = envWith({ ENVIRONMENT: 'production', ACCESS_TEAM_DOMAIN: 'https://team.cloudflareaccess.com' })
const prod = loadConfig(prodEnv)
const url = 'https://localhost/api/me'

function req(headers: Record<string, string>, u = url) {
  return new Request(u, { headers })
}

describe('Access JWT verification', () => {
  it('accepts a valid user token and reads the email', async () => {
    const k = await makeAccessKeys()
    const jwt = await k.sign({ email: 'me@example.com' })
    expect(await authenticate(req({ 'cf-access-jwt-assertion': jwt }), prod, { jwks: k.jwks })).toEqual({
      identity: 'me@example.com',
      kind: 'user',
    })
  })

  it('maps a service token (common_name, no email) to a service identity', async () => {
    const k = await makeAccessKeys()
    const jwt = await k.sign({ common_name: 'abc123.access' })
    expect(await authenticate(req({ 'cf-access-jwt-assertion': jwt }), prod, { jwks: k.jwks })).toEqual({
      identity: 'abc123.access',
      kind: 'service',
    })
  })

  it('rejects a wrong audience', async () => {
    const k = await makeAccessKeys()
    const jwt = await k.sign({ email: 'me@example.com' }, { aud: 'other-app' })
    expect(await authenticate(req({ 'cf-access-jwt-assertion': jwt }), prod, { jwks: k.jwks })).toBeNull()
  })

  it('rejects a wrong issuer', async () => {
    const k = await makeAccessKeys()
    const jwt = await k.sign({ email: 'me@example.com' }, { iss: 'https://evil.cloudflareaccess.com' })
    expect(await authenticate(req({ 'cf-access-jwt-assertion': jwt }), prod, { jwks: k.jwks })).toBeNull()
  })

  it('rejects an expired token', async () => {
    const k = await makeAccessKeys()
    const jwt = await k.sign({ email: 'me@example.com' }, { exp: Math.floor(Date.now() / 1000) - 3600 })
    expect(await authenticate(req({ 'cf-access-jwt-assertion': jwt }), prod, { jwks: k.jwks })).toBeNull()
  })

  it('rejects a token signed by an unknown key', async () => {
    const trusted = await makeAccessKeys()
    const attacker = await makeAccessKeys()
    const jwt = await attacker.sign({ email: 'me@example.com' })
    expect(await authenticate(req({ 'cf-access-jwt-assertion': jwt }), prod, { jwks: trusted.jwks })).toBeNull()
  })

  it('rejects a missing header and garbage', async () => {
    const k = await makeAccessKeys()
    expect(await authenticate(req({}), prod, { jwks: k.jwks })).toBeNull()
    expect(await authenticate(req({ 'cf-access-jwt-assertion': 'not.a.jwt' }), prod, { jwks: k.jwks })).toBeNull()
  })

  it('rejects a token with neither email nor common_name', async () => {
    const k = await makeAccessKeys()
    const jwt = await k.sign({ sub: 'x' })
    expect(await authenticate(req({ 'cf-access-jwt-assertion': jwt }), prod, { jwks: k.jwks })).toBeNull()
  })
})

describe('dev bypass', () => {
  it('is honoured in dev/test on a loopback hostname', async () => {
    const cfg = loadConfig(baseEnv)
    expect(cfg.environment).toBe('test')
    const r = await authenticate(req({ 'x-dev-identity': 'dev@example.com' }, 'http://localhost/api/me'), cfg)
    expect(r).toEqual({ identity: 'dev@example.com', kind: 'user' })
  })

  it('is ignored when ENVIRONMENT=production, even on localhost', async () => {
    const r = await authenticate(req({ 'x-dev-identity': 'dev@example.com' }, 'http://localhost/api/me'), prod)
    expect(r).toBeNull()
  })

  it('is ignored for non-loopback hostnames even in dev', async () => {
    const cfg = loadConfig(envWith({ ENVIRONMENT: 'dev', APP_HOST: 'cloud.example.com' }))
    const r = await authenticate(req({ 'x-dev-identity': 'dev@example.com' }, 'https://cloud.example.com/api/me'), cfg)
    expect(r).toBeNull()
  })
})

describe('config validation', () => {
  it('throws, naming the variable, when Access settings are missing', () => {
    expect(() => loadConfig(envWith({ ACCESS_AUD: '' }))).toThrow(/ACCESS_AUD/)
    expect(() => loadConfig(envWith({ ACCESS_TEAM_DOMAIN: undefined }))).toThrow(/ACCESS_TEAM_DOMAIN/)
  })
  it('rejects an unknown environment and identical hosts', () => {
    expect(() => loadConfig(envWith({ ENVIRONMENT: 'staging' }))).toThrow()
    expect(() => loadConfig(envWith({ SHARE_HOST: 'localhost' }))).toThrow()
  })
})
