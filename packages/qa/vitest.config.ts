import { defineConfig } from 'vitest/config';

export default defineConfig({
  test: {
    exclude: ['**/node_modules/**', '**/dist/**', '**/.git/**', 'public-storage.test.mjs', 'ts-register.test.mjs', 'src/apps-contract.test.ts'],
  },
});
