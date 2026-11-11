import { z } from 'zod'
import { authedGet, authedPost } from './auth'
import * as shares from '../server/services/shares'

const idInput = z.object({ id: z.string().length(26) })

export const listShares = authedGet
  .validator(idInput)
  .handler(({ context, data }) => shares.listShares(context.services, data.id))

export const createShare = authedPost
  .validator(
    idInput.extend({
      expires_in_seconds: z
        .number()
        .int()
        .min(60)
        .max(10 * 365 * 86_400)
        .nullable()
        .optional(),
      allow_download: z.boolean().optional(),
    }),
  )
  .handler(({ context, data }) => {
    const { id, ...input } = data
    return shares.createShare(context.services, context.identity, id, input)
  })

export const revokeShare = authedPost
  .validator(idInput)
  .handler(async ({ context, data }) => {
    await shares.revokeShare(context.services, data.id)
    return { ok: true as const }
  })
