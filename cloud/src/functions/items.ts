import { z } from 'zod'
import { authedGet, authedPost } from './auth'
import { ITEM_KINDS } from '../server/db/items'
import * as items from '../server/services/items'

const listInput = z.object({
  q: z.string().max(200).optional(),
  kind: z.enum(ITEM_KINDS).optional(),
  favorite: z.boolean().optional(),
  cursor: z.string().max(200).optional(),
  limit: z.number().int().min(1).max(100).optional(),
})
const idInput = z.object({ id: z.string().length(26) })

export const listItems = authedGet
  .validator(listInput)
  .handler(({ context, data }) => items.listItems(context.services, data))

export const getItem = authedGet
  .validator(idInput)
  .handler(({ context, data }) => items.getItemWithShares(context.services, data.id))

export const updateItem = authedPost
  .validator(
    idInput.extend({
      title: z.string().trim().min(1).max(300).optional(),
      favorite: z.boolean().optional(),
    }),
  )
  .handler(({ context, data }) => {
    const { id, ...patch } = data
    return items.patchItem(context.services, id, patch)
  })

export const deleteItem = authedPost
  .validator(idInput)
  .handler(async ({ context, data }) => {
    await items.deleteItem(context.services, data.id)
    return { ok: true as const }
  })
