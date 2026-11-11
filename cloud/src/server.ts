/**
 * Worker entry. Every request passes dispatch() (hostname split, origin check, Access JWT
 * verification) before TanStack Start, the API or a static asset sees it.
 */
import startHandler from '@tanstack/react-start/server-entry'
import { dispatch, runScheduled } from './server/dispatch'
import type { AppEnv } from './server/env'

export default {
  async fetch(request: Request, env: AppEnv, ctx: ExecutionContext): Promise<Response> {
    return dispatch(request, env, ctx, { next: (req) => startHandler.fetch(req) })
  },
  async scheduled(_controller: ScheduledController, env: AppEnv, ctx: ExecutionContext): Promise<void> {
    ctx.waitUntil(runScheduled(env))
  },
}
