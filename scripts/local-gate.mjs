#!/usr/bin/env node
// Mandatory local gate (Adrian, 2026-10-22: "local deterministic checks ... mandatory for all builds"), run by
// .githooks/pre-push. RightKit runs the same shipped gate every app runs: packages/release/bin/rightkit-gate.mjs
// (@rightkit/release `rightkit-gate`) in explicit repository pre-push mode: class A, focused scripts/ci tests & generated docs.
// Native compilation, lint, rustdoc & package tests remain publication qualification requirements.
//
// Usage: node scripts/local-gate.mjs [--base <ref>] [--plan]   (default base: origin/main)
import { main } from "../packages/release/bin/rightkit-gate.mjs";

process.exit(await main(["--pre-push", ...process.argv.slice(2)]));
