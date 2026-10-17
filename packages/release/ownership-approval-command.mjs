#!/usr/bin/env node
// `right-release ownership-approval` is retired (Orthic-Labs/rightkit#262), under the proportional-trust rule, like trust-sync.
// Ownership exceptions are plain entries in the repository's .rightkit/ownership-exceptions.json, reviewed through git:
// no drafts, signatures, Touch ID or native helper. The command is a no-op that says so and always succeeds.
import { realpathSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

export const OWNERSHIP_APPROVAL_RETIRED = "right-release ownership-approval is retired: signed ownership exceptions (draft, sign, Touch ID and native-helper paths) no longer exist. Edit .rightkit/ownership-exceptions.json directly, with plain entries {capability, package, reason, owner, date}, and review the change through git. `right-release ownership` marks a matching finding excepted and warns about stale entries.";

export function main(_args = [], { print = (line) => process.stdout.write(`${line}\n`) } = {}) {
  print(`[right-release] ${OWNERSHIP_APPROVAL_RETIRED}`);
  return 0;
}
const realOrSelf = (file) => { try { return realpathSync(file); } catch { return path.resolve(file); } };
if (process.argv[1] && realOrSelf(process.argv[1]) === realOrSelf(fileURLToPath(import.meta.url))) process.exitCode = main(process.argv.slice(2));
