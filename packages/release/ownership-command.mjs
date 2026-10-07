#!/usr/bin/env node
// right-release ownership: check an app against RightKit's capability ownership, or rebuild
// the shipped manifest from each package's declaration.
//
//   right-release ownership [--app <dir>] [--json]
//   right-release ownership --write-manifest [--rightkit <tools/rightkit>]
import { readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { aggregateOwnership, formatTable, MANIFEST_FILE } from "./ownership.mjs";
import { createOwnershipEvidence } from "./ownership-evidence.mjs";
import { spawnSync } from "./background-process.mjs";

const packageRoot = path.dirname(fileURLToPath(import.meta.url));
const shipped = path.join(packageRoot, MANIFEST_FILE);

function flag(args, name) {
  const i = args.indexOf(name);
  return i >= 0 ? args[i + 1] : undefined;
}

export async function main(args) {
  if (args.includes("--write-manifest")) {
    const root = path.resolve(flag(args, "--rightkit") ?? path.join(packageRoot, "..", ".."));
    const manifest = aggregateOwnership(root);
    writeFileSync(shipped, `${JSON.stringify(manifest, null, 2)}\n`);
    const { syncOwnershipCatalog } = await import("../build-control-clients/src/ownership-catalog-sync.mjs");
    syncOwnershipCatalog({ source: packageRoot });
    console.log(`[right-release] ownership manifest: ${manifest.capabilities.length} capabilities -> ${shipped}`);
    return 0;
  }
  const app = path.resolve(flag(args, "--app") ?? process.cwd());
  const top = spawnSync("git", ["-C", app, "rev-parse", "--show-toplevel"], { encoding: "utf8", windowsHide: true });
  if (top.status !== 0) return failClosed(args, "repository", "could not resolve --app to a Git repository");
  const repoRoot = path.resolve(String(top.stdout).trim());
  let checked;
  try { checked = createOwnershipEvidence(repoRoot, app); }
  catch (error) { return failClosed(args, "protected-evidence", error.message); }
  const result = checked.result;
  if (args.includes("--json")) {
    const error = !checked.floor.ok
      ? { type: "ownership-floor", message: checked.floor.problems.join("; ") }
      : result.coverage.length
        ? { type: "ownership-coverage", message: `${result.coverage.length} ownership coverage gap(s)` }
        : undefined;
    console.log(JSON.stringify({ ok: checked.ok, ...(error ? { error } : {}), rows: result.rows, violations: result.violations, covered: result.covered, problems: [...result.problems, ...checked.floor.problems], coverage: result.coverage, metadata: result.metadata }, null, 2));
  } else {
    console.log(`RightKit ownership: ${path.basename(app)}`);
    console.log(formatTable(result.rows));
    for (const v of result.violations) {
      console.log(`  OWN COPY ${v.fingerprint} (${v.package}) ${v.file}${v.line ? `:${v.line}` : ""}  ${v.text}`);
    }
    for (const p of result.problems) console.log(`  EXCEPTION ${p}`);
    console.log(checked.ok ? "[right-release] ownership passed" : `[right-release] ownership FAILED: ${[...result.problems, ...checked.floor.problems].join("; ") || "move these to RightKit or add a reviewed, dated exception"}`);
  }
  return checked.ok ? 0 : 1;
}

function failClosed(args, type, message) {
  if (args.includes("--json")) console.log(JSON.stringify({ ok: false, error: { type, message }, rows: [], violations: [], covered: [], problems: [message], coverage: [], metadata: null }, null, 2));
  else console.log(`[right-release] ownership FAILED: ${message}`);
  return 1;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main(process.argv.slice(2)).then((code) => {
    process.exitCode = code;
  }).catch((error) => {
    console.error(`[right-release] ownership failed: ${error.message}`);
    process.exitCode = 1;
  });
}
