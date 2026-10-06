import { defineConfig } from "vitest/config";

export default defineConfig({
  resolve: { dedupe: ["react", "react-dom"] },
  test: { environment: "node", include: ["e2e/**/*.test.ts"], testTimeout: 30_000, fileParallelism: false },
});
