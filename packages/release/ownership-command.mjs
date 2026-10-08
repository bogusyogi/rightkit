#!/usr/bin/env node
// right-release ownership: check an app against RightKit's capability ownership, or rebuild
// the shipped manifest from each package's declaration.
//
//   right-release ownership [--app <dir>] [--json]
//   right-release ownership --write-manifest [--rightkit <tools/rightkit>] [--approve-new-capabilities]
// --approve-new-capabilities permits new ids during local generation only.
// Existing owner changes are always refused; installation needs protected authorization.
import { realpathSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { formatTable } from "./ownership.mjs";
import { writeOwnershipCatalog } from "./ownership-catalog.mjs";
import { createOwnershipEvidence } from "./ownership-evidence.mjs";
import { spawnSync } from "./background-process.mjs";
import { main as stagedMain } from "./ownership-staged.mjs";

const packageRoot = path.dirname(fileURLToPath(import.meta.url));

function flag(args, name) {
  const i = args.indexOf(name);
  return i >= 0 ? args[i + 1] : undefined;
}

export async function main(args, { evidenceFactory = createOwnershipEvidence, gitRunner = spawnSync, print = (value) => console.log(value) } = {}) {
  if (args.includes("--help")) {
    print("right-release ownership [--app <dir>] [--json]\nright-release ownership --staged [--app <dir>] [--json]  (pre-commit gate: scans the index, not the working tree)\nright-release ownership --write-manifest [--rightkit <dir>] [--approve-new-capabilities]\n--approve-new-capabilities permits new capability ids during local generation only. Existing owner conflicts are always refused. Protected installation authorization remains separate.");
    return 0;
  }
  if (args.includes("--staged")) return stagedMain(args, { print });
  if (["enroll", "approve-key", "revoke-key"].includes(args[0])) {
    const { main: keyMain } = await import("./ownership-key-command.mjs");
    return keyMain(args, { runner: gitRunner, print });
  }
  if (args.includes("--write-manifest")) {
    const root = path.resolve(flag(args, "--rightkit") ?? path.join(packageRoot, "..", ".."));
    const manifest = writeOwnershipCatalog(root, { approveNewCapabilities: args.includes("--approve-new-capabilities") });
    const { syncConsumerCopies } = await import("./rightkit-sync.mjs");
    syncConsumerCopies(root);
    print(`[right-release] ownership manifest: ${manifest.capabilities.length} owner rows -> ${path.join(root, "packages/release/rightkit-ownership.json")}`);
    return 0;
  }
  const app = path.resolve(flag(args, "--app") ?? process.cwd());
  const top = gitRunner("git", ["-C", app, "rev-parse", "--show-toplevel"], { encoding: "utf8", windowsHide: true });
  if (top.status !== 0) return failClosed(args, "repository", "could not resolve --app to a Git repository");
  const repoRoot = path.resolve(String(top.stdout).trim());
  let checked;
  try { checked = evidenceFactory(repoRoot, app); }
  catch (error) { return failClosed(args, "protected-evidence", error.message, print); }
  const result = checked.result;
  if (args.includes("--json")) {
    const error = checked.enrollmentPending ? undefined : !checked.floor.ok
      ? { type: "ownership-floor", message: checked.floor.problems.join("; ") }
      : result.coverage.length
        ? { type: "ownership-coverage", message: `${result.coverage.length} ownership coverage gap(s)` }
        : undefined;
    print(JSON.stringify({ ok: checked.ok, ...(checked.enrollmentPending ? { enrollmentPending: true, warning: checked.warning } : {}), ...(error ? { error } : {}), rows: result.rows, violations: result.violations, covered: result.covered, problems: [...result.problems, ...checked.floor.problems], coverage: result.coverage, metadata: result.metadata }, null, 2));
  } else {
    print(`RightKit ownership: ${path.basename(app)}`);
    print(formatTable(result.rows));
    for (const v of result.violations) {
      print(`  OWN COPY ${v.fingerprint} (${v.package}) ${v.file}${v.line ? `:${v.line}` : ""}  ${v.text}`);
    }
    for (const p of result.problems) print(`  ${checked.enrollmentPending ? "DIAGNOSTIC" : "EXCEPTION"} ${p}`);
    print(checked.enrollmentPending ? `[right-release] ${checked.warning}` : checked.ok ? "[right-release] ownership passed" : `[right-release] ownership FAILED: ${[...result.problems, ...checked.floor.problems].join("; ") || "move these to RightKit or add a reviewed, dated exception"}`);
  }
  return checked.ok ? 0 : 1;
}

function failClosed(args, type, message, print = (value) => console.log(value)) {
  if (args.includes("--json")) print(JSON.stringify({ ok: false, error: { type, message }, rows: [], violations: [], covered: [], problems: [message], coverage: [], metadata: null }, null, 2));
  else print(`[right-release] ownership FAILED: ${message}`);
  return 1;
}

// Compare real paths: a symlinked spelling of this file (pnpm links, macOS /var -> /private/var)
// must still run the command.
const isEntry = (() => {
  if (!process.argv[1]) return false;
  try { return realpathSync(process.argv[1]) === realpathSync(fileURLToPath(import.meta.url)); }
  catch { return path.resolve(process.argv[1]) === fileURLToPath(import.meta.url); }
})();
if (isEntry) {
  main(process.argv.slice(2)).then((code) => {
    process.exitCode = code;
  }).catch((error) => {
    console.error(`[right-release] ownership failed: ${error.message}`);
    process.exitCode = 1;
  });
}
