import { fileURLToPath } from "node:url";
import { defineConfig } from "vitest/config";

export default defineConfig({
  oxc: { jsx: { runtime: "automatic" } },
  test: {
    include: ["src/**/*.test.{ts,tsx,js,jsx}", "test/**/*.test.{ts,tsx,js,jsx}"],
    environment: "happy-dom",
    setupFiles: ["./src/test/setup.ts"],
  },
  resolve: {
    // Hosts alias @rightkit-host to their project root; tests use a fixture.
    alias: { "@rightkit-host": fileURLToPath(new URL("./test/host-fixture", import.meta.url)) },
  },
});
