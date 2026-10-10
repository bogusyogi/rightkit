import { defineConfig } from "vitest/config";

export default defineConfig({
  test: { environment: "node", include: ["e2e/**/*.test.ts", "src/**/*.test.ts"], testTimeout: 60_000, fileParallelism: false },
});
