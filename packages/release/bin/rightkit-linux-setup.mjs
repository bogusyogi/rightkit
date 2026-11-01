#!/usr/bin/env node
// rightkit-linux-setup [--root <app>] [--dry-run]: provision a Linux host (cloud container, VM, WSL2) for `rightkit-host-gate`.
import { spawnSync } from "../background-process.mjs";
import { setup } from "../linux-setup.mjs";

const args = process.argv.slice(2);
if (args.includes("--version")) { console.log("rightkit-linux-setup (@rightkit/release)"); process.exit(0); }
if (process.platform !== "linux") { console.error("rightkit-linux-setup runs on Linux only"); process.exit(2); }
const at = args.indexOf("--root");
const run = (cmd, a, o) => spawnSync(cmd, a, { encoding: "utf8", stdio: ["ignore", "pipe", "inherit"], ...o });
const fetchText = (url) => { const r = run("curl", ["-fsSL", url]); if (r.status !== 0) throw new Error(`fetch ${url} failed`); return r.stdout; };
try {
  const r = setup({ root: at >= 0 ? args[at + 1] : process.cwd(), run, fetchText, dryRun: args.includes("--dry-run"), log: (m) => console.error(`[linux-setup] ${m}`) });
  if (args.includes("--dry-run")) console.log(r.steps.join("\n"));
} catch (e) { console.error(`rightkit-linux-setup: ${e.message}`); process.exit(1); }
