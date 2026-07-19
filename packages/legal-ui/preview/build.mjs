#!/usr/bin/env node
// Bundles the preview harness (React + LegalGate + all ten variants) into a
// single self-contained script. No CDN, no network dependency at preview
// time — everything ships in preview/dist/entry.js. Local-only; preview/ is
// excluded from the published package.
import { build } from "esbuild";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const here = dirname(fileURLToPath(import.meta.url));

await build({
  entryPoints: [join(here, "entry.tsx")],
  outfile: join(here, "dist", "entry.js"),
  bundle: true,
  format: "iife",
  target: "es2022",
  jsx: "automatic",
  loader: { ".tsx": "tsx", ".ts": "ts" },
  logLevel: "info",
});
