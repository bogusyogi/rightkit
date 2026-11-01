#!/usr/bin/env node
// The publish route's own contract (publish.yml's plan job), as one command. publish.yml runs it before every publication and
// ci.yml runs it on every push and pull request, so no merge can leave publishing red without CI showing it first.
import { spawnSync } from "node:child_process";

export const PUBLISH_ROUTE_CHECKS = Object.freeze([
  ["node", ["scripts/ci/workflow-contract.mjs", ".github/workflows/publish.yml"]],
  ["node", ["--test", "scripts/ci/publish.test.mjs", "scripts/ci/select.test.mjs", "scripts/ci/test-results.test.mjs", "scripts/ci/scan-receipt.test.mjs"]],
  ["node", ["scripts/ci/reviewed-workflow-contract.mjs"]],
  ["node", ["--test", "scripts/ci/reviewed-candidate.test.mjs", "scripts/ci/registry-consumer.test.mjs", "scripts/ci/workflow-contract-portability.test.mjs",
    "scripts/ci/consumer-recipes.test.mjs", "scripts/ci/semantic-admission.test.mjs"]],
]);

if (import.meta.url === `file://${process.argv[1]}`) {
  for (const [command, args] of PUBLISH_ROUTE_CHECKS) {
    console.log(`publish-preflight: ${command} ${args.join(" ")}`);
    const result = spawnSync(command, args, { stdio: "inherit" });
    if (result.status !== 0) { console.error(`publish-preflight: failed: ${command} ${args.join(" ")}`); process.exit(result.status ?? 1); }
  }
  console.log("publish-preflight: the publish route's own contract passes");
}
