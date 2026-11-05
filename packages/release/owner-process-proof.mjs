#!/usr/bin/env node
// rightkit#1511: prints the process-only proof of one dead owner record as JSON (build-ownership.mjs proveOwnerProcessesGone). Writes nothing.
// usage: node owner-process-proof.mjs <managed root>/build-ownership/<key>/owner.json
// exit 0 with the proof on stdout; exit 2 with the reason on stderr when a process is still live or unknown; exit 64 on bad usage.
import { proveOwnerProcessesGone } from "./build-ownership.mjs";

const file = process.argv[2];
if (!file || process.argv.length !== 3) { process.stderr.write("usage: owner-process-proof.mjs <owner.json>\n"); process.exit(64); }
try {
  process.stdout.write(`${JSON.stringify(proveOwnerProcessesGone({ file }), null, 2)}\n`);
} catch (e) {
  process.stderr.write(`${e.message}\n`);
  process.exit(2);
}
