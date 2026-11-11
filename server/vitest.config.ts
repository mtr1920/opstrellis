import { defineConfig } from 'vitest/config';

export default defineConfig({
  test: {
    include: ['test/**/*.test.ts'],
    globalSetup: ['test/global-setup.ts'],
    // Files share one database; run them one at a time.
    fileParallelism: false,
    testTimeout: 30_000,
    hookTimeout: 60_000,
    env: {
      DATABASE_URL: process.env.TEST_DATABASE_URL ?? 'postgres://postgres:postgres@localhost:5432/opstrellis_test',
      PUBLIC_URL: 'http://localhost:8787',
      TRUST_PROXY: '1',
      MASTER_KEY: 'test-master-key',
      STRIPE_WEBHOOK_SECRET: 'whsec_test_secret',
      STRIPE_SECRET_KEY: process.env.STRIPE_SECRET_KEY ?? '',
      LOCAL_MODEL_DIR: 'models',
      WEB_DIR: 'dist/web',
    },
  },
});
