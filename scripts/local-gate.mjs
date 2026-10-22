#!/usr/bin/env node
// Mandatory local gate (Adrian, 2026-10-22: "local deterministic checks ... mandatory for all builds"), run by
// .githooks/pre-push. RightKit runs the same shipped gate every app runs: packages/release/bin/rightkit-gate.mjs
// (@rightkit/release `rightkit-gate`). One gate, one scan module; nothing here is RightKit-only.
//
// Usage: node scripts/local-gate.mjs [--base <ref>] [--plan]   (default base: origin/main)
import { main } from "../packages/release/bin/rightkit-gate.mjs";

process.exit(await main(process.argv.slice(2)));
