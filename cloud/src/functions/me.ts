import { authedGet } from './auth'
import { limits } from '../server/services/uploads'

export const getMe = authedGet.handler(async ({ context }) => ({
  identity: context.identity.identity,
  kind: context.identity.kind,
  limits: limits(context.services),
}))
