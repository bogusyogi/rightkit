import { fileURLToPath } from "node:url";
import { defineConfig } from "vitest/config";

export default defineConfig({
  oxc: { jsx: { runtime: "automatic" } },
  test: {
    include: ["src/**/*.test.{ts,tsx,js,jsx}", "test/**/*.test.{ts,tsx,js,jsx}"],
    // node:test files run under `node --test` (see the test script).
    exclude: ["test/queue/already-queued-notice.test.ts", "test/queue/category-display.test.ts", "test/queue/detail-find-interaction.test.ts", "test/queue/drop-overlay.test.ts", "test/queue/engine-stream.test.ts", "test/queue/image-downloads.test.ts", "test/queue/job-display-stage.test.ts", "test/queue/media-kind.test.ts", "test/queue/playlist-url.test.ts", "test/queue/queue-library-lane.test.ts", "test/queue/transcript-hooks.test.ts", "test/queue/transcript-load.test.ts"],
    environment: "node",
  },
  resolve: {
    // Hosts alias @rightkit-host to their project root; tests use a fixture.
    alias: { "@rightkit-host": fileURLToPath(new URL("./test/host-fixture", import.meta.url)) },
  },
});
