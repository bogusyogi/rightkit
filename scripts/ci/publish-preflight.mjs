#!/usr/bin/env node
// The publish route's own contract (publish.yml's plan job), as one command. publish.yml runs it before every publication and
// ci.yml runs it on every push and pull request, so no merge can leave publishing red without CI showing it first.
import { spawnSync } from "node:child_process";
import { realpathSync } from "node:fs";
import { fileURLToPath } from "node:url";

export const PUBLISH_ROUTE_CHECKS = Object.freeze([
  ["node", ["scripts/ci/workflow-contract.mjs", ".github/workflows/publish.yml"]],
  ["node", ["scripts/ci/reviewed-workflow-contract.mjs"]],
]);

if (process.argv[1] && realpathSync(process.argv[1]) === realpathSync(fileURLToPath(import.meta.url))) {
  for (const [command, args] of PUBLISH_ROUTE_CHECKS) {
    console.log(`publish-preflight: ${command} ${args.join(" ")}`);
    const result = spawnSync(command, args, { stdio: "inherit" });
    if (result.status !== 0) { console.error(`publish-preflight: failed: ${command} ${args.join(" ")}`); process.exit(result.status ?? 1); }
  }
  console.log("publish-preflight: the publish route's own contract passes");
}
