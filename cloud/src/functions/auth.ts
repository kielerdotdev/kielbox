/**
 * The only place `createServerFn(` appears (a test enforces it). Every server function is
 * built from `authedGet` / `authedPost`, so each one re-verifies the Access identity itself:
 * the second layer behind the gate in `src/server.ts`.
 */
import { createMiddleware, createServerFn } from '@tanstack/react-start'
import { getRequest } from '@tanstack/react-start/server'
import { env } from 'cloudflare:workers'
import { authenticate } from '../server/auth/access'
import { buildServices } from '../server/context'
import { loadConfig, type AppEnv } from '../server/env'

export const UNAUTHORIZED_MESSAGE = 'unauthorized'

export const authMiddleware = createMiddleware({ type: 'function' }).server(async ({ next }) => {
  const appEnv = env as unknown as AppEnv
  const request = getRequest()
  const config = loadConfig(appEnv)
  const identity = await authenticate(request, config)
  if (!identity) throw new Error(UNAUTHORIZED_MESSAGE)
  return next({
    context: { identity, services: buildServices(appEnv, new URL(request.url), config) },
  })
})

/** Reads. */
export const authedGet = createServerFn({ method: 'GET' }).middleware([authMiddleware])
/** Mutations: never GET. */
export const authedPost = createServerFn({ method: 'POST' }).middleware([authMiddleware])
