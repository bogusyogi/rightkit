#!/usr/bin/env node
// right-release ownership: check an app against RightKit's capability ownership, or rebuild
// the shipped manifest from each package's declaration.
//
//   right-release ownership [--app <dir>] [--json]
//   right-release ownership --write-manifest [--rightkit <tools/rightkit>]
import { readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { aggregateOwnership, checkApp, formatTable, MANIFEST_FILE } from "./ownership.mjs";

const packageRoot = path.dirname(fileURLToPath(import.meta.url));
const shipped = path.join(packageRoot, MANIFEST_FILE);

function flag(args, name) {
  const i = args.indexOf(name);
  return i >= 0 ? args[i + 1] : undefined;
}

export function main(args) {
  if (args.includes("--write-manifest")) {
    const root = path.resolve(flag(args, "--rightkit") ?? path.join(packageRoot, "..", ".."));
    const manifest = aggregateOwnership(root);
    writeFileSync(shipped, `${JSON.stringify(manifest, null, 2)}\n`);
    console.log(`[right-release] ownership manifest: ${manifest.capabilities.length} capabilities -> ${shipped}`);
    return 0;
  }
  const app = path.resolve(flag(args, "--app") ?? process.cwd());
  const manifest = JSON.parse(readFileSync(shipped, "utf8"));
  const result = checkApp(app, manifest);
  if (args.includes("--json")) {
    console.log(JSON.stringify({ ok: result.ok, rows: result.rows, violations: result.violations, problems: result.problems }, null, 2));
  } else {
    console.log(`RightKit ownership: ${path.basename(app)}`);
    console.log(formatTable(result.rows));
    for (const v of result.violations) {
      console.log(`  OWN COPY ${v.fingerprint} (${v.package}) ${v.file}${v.line ? `:${v.line}` : ""}  ${v.text}`);
    }
    for (const p of result.problems) console.log(`  EXCEPTION ${p}`);
    console.log(result.ok ? "[right-release] ownership passed" : "[right-release] ownership FAILED: move these to RightKit or add a reviewed, dated exception");
  }
  return result.ok ? 0 : 1;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  process.exitCode = main(process.argv.slice(2));
}
