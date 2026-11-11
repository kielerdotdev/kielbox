import { defineConfig } from 'vitest/config'
import { cloudflareTest, readD1Migrations } from '@cloudflare/vitest-plugin'

export default defineConfig(async () => {
  const migrations = await readD1Migrations('./migrations')
  return {
    plugins: [
      cloudflareTest({
        miniflare: {
          compatibilityDate: '2025-09-02',
          compatibilityFlags: ['nodejs_compat'],
          d1Databases: ['DB'],
          r2Buckets: ['BUCKET'],
          bindings: {
            TEST_MIGRATIONS: migrations,
            ENVIRONMENT: 'test',
            APP_HOST: 'localhost',
            SHARE_HOST: '127.0.0.1',
            ACCESS_TEAM_DOMAIN: 'https://team.cloudflareaccess.com',
            ACCESS_AUD: 'test-aud',
            R2_ACCOUNT_ID: 'acct',
            R2_BUCKET_NAME: 'kielbox-cloud',
            LOCAL_SIGNING_KEY: 'test-signing-key',
            // Small multipart sizes keep the tests fast; Miniflare enforces R2's 5 MiB part minimum.
            MULTIPART_THRESHOLD_BYTES: String(8 * 1024 * 1024),
            PART_SIZE_MIN_BYTES: String(5 * 1024 * 1024),
          },
        },
      }),
    ],
    test: {
      include: ['test/**/*.test.ts'],
      setupFiles: ['./test/setup.ts'],
    },
  }
})
