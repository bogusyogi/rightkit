import { defineConfig } from "vitest/config";

export default defineConfig({
  resolve: { dedupe: ["react", "react-dom"] },
  test: { environment: "node", include: ["e2e/**/*.test.ts", "src/**/*.test.ts", "src/**/*.test.tsx"], testTimeout: 30_000, hookTimeout: 60_000, fileParallelism: false },
});
