import { z } from 'zod'
import { ITEM_KINDS } from './db/items'

const noControl = (s: string) => !/[\u0000-\u001f\u007f]/.test(s)
const byteLength = (s: string) => new TextEncoder().encode(s).length

export const createUploadSchema = z.object({
  filename: z
    .string()
    .min(1)
    .refine((s) => byteLength(s) <= 255, 'filename must be at most 255 bytes')
    .refine(noControl, 'filename must not contain control characters'),
  size: z.number().int().min(1),
  content_type: z.string().min(1).max(100),
  kind: z.enum(ITEM_KINDS),
  title: z.string().trim().min(1).max(300).refine(noControl, 'title must not contain control characters').optional(),
  captured_at: z.number().int().min(0).max(8.64e15),
  width: z.number().int().min(1).max(100_000).optional(),
  height: z.number().int().min(1).max(100_000).optional(),
  duration_ms: z.number().int().min(0).max(31_536_000_000).optional(),
  thumbnail: z.boolean().optional(),
})
export type CreateUploadInput = z.infer<typeof createUploadSchema>

export const partsRequestSchema = z.object({
  part_numbers: z.array(z.number().int().min(1).max(10_000)).min(1).max(32),
})

export const completeSchema = z.object({
  parts: z
    .array(z.object({ part_number: z.number().int().min(1).max(10_000), etag: z.string().min(1).max(200) }))
    .max(10_000)
    .optional(),
})

export const listQuerySchema = z.object({
  limit: z.coerce.number().int().min(1).max(100).default(50),
  cursor: z.string().max(200).optional(),
  q: z.string().max(200).optional(),
  kind: z.enum(ITEM_KINDS).optional(),
  favorite: z.enum(['1', 'true']).optional(),
})

export const patchItemSchema = z
  .object({
    title: z.string().trim().min(1).max(300).refine(noControl, 'title must not contain control characters').optional(),
    favorite: z.boolean().optional(),
  })
  .refine((v) => v.title !== undefined || v.favorite !== undefined, 'nothing to update')

export const createShareSchema = z.object({
  expires_in_seconds: z
    .number()
    .int()
    .min(60)
    .max(10 * 365 * 86_400)
    .nullable()
    .optional(),
  allow_download: z.boolean().optional(),
})
