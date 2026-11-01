import { defineConfig } from "vitest/config";

export default defineConfig({
  oxc: { jsx: { runtime: "automatic" } },
  test: {
    include: ["src/**/*.test.{ts,tsx,js,jsx}", "test/**/*.test.{ts,tsx,js,jsx}"],
    // node:test files run under `node --test` (see the test script).
    exclude: ["test/vcard.test.ts"],
    environment: "node",
  },
});
