import { applyD1Migrations } from 'cloudflare:test'
import { env } from 'cloudflare:workers'
import { beforeEach } from 'vitest'
import type { D1Migration } from '@cloudflare/vitest-plugin'

const e = env as unknown as { DB: D1Database; TEST_MIGRATIONS: D1Migration[] }
await applyD1Migrations(e.DB, e.TEST_MIGRATIONS)

// Each test starts from empty tables and an empty bucket.
beforeEach(async () => {
  await e.DB.batch([e.DB.prepare('DELETE FROM shares'), e.DB.prepare('DELETE FROM items')])
  const bucket = (env as unknown as { BUCKET: R2Bucket }).BUCKET
  let cursor: string | undefined
  do {
    const page = await bucket.list({ cursor })
    if (page.objects.length) await bucket.delete(page.objects.map((o) => o.key))
    cursor = page.truncated ? page.cursor : undefined
  } while (cursor)
})
