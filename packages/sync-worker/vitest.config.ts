import { fileURLToPath } from 'node:url';
import { defineWorkersConfig, readD1Migrations } from '@cloudflare/vitest-pool-workers/config';

export default defineWorkersConfig(async () => {
  const migrations = await readD1Migrations(fileURLToPath(new URL('./migrations', import.meta.url)));
  return {
    test: {
      include: ['test/**/*.test.ts'],
      exclude: ['**/node_modules/**', '**/dist/**', '**/.git/**'],
      setupFiles: ['./test/apply-migrations.ts'],
      poolOptions: {
        workers: {
          wrangler: { configPath: './wrangler.toml' },
          miniflare: {
            // Test-only values. Production secrets come from `wrangler secret put`.
            bindings: {
              TEST_MIGRATIONS: migrations,
              SYNC_JWT_SECRET: 'test-secret-current-0123456789abcdef',
              SYNC_JWT_SECRET_PREVIOUS: 'test-secret-previous-0123456789abcdef',
              ALLOWED_APPS: 'heardright,otherapp',
              RATE_LIMIT_PER_MINUTE: '25',
              ACCOUNT_STORAGE_CAP_BYTES: '300000',
              TOMBSTONE_COMPACT_DAYS: '90',
              BLOB_SPILL_BYTES: '65536',
              SHARD_COUNT: '4',
              CF_PLAN: 'free',
            },
          },
        },
      },
    },
  };
});
