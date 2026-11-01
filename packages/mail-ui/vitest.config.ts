import { fileURLToPath } from "node:url";
import { defineConfig } from "vitest/config";

export default defineConfig({
  oxc: { jsx: { runtime: "automatic" } },
  test: {
    include: ["src/**/*.test.{ts,tsx,js,jsx}", "test/**/*.test.{ts,tsx,js,jsx}"],
    // node:test files run under `node --test` (see the test script).
    exclude: ["test/attachmentTypes.test.ts", "test/auth.test.ts", "test/compactionMessage.test.ts", "test/composeActions.test.ts", "test/emailFrameShortcuts.test.ts", "test/folderActions.test.ts", "test/highlightSnippet.test.ts", "test/imageTrust.test.ts", "test/openThreadAction.test.ts", "test/readerToolbarCss.test.ts", "test/refreshTail.test.ts", "test/savedSearches.test.ts", "test/searchCoverage.test.ts", "test/searchPaging.test.ts", "test/searchRouting.test.ts", "test/semanticStatus.test.ts", "test/syncReloadGate.test.ts", "test/threadLoadGate.test.ts"],
    environment: "node",
  },
  resolve: {
    // Hosts alias @rightkit-host to their project root; tests use a fixture.
    alias: { "@rightkit-host": fileURLToPath(new URL("./test/host-fixture", import.meta.url)) },
  },
});
