#!/usr/bin/env node
// One-shot migration (Adrian, 2026-10-24): RustRight is the broker client's name (rustright#91, 2026-10-17); `rightkit` was
// kept as a deprecated alias of the same binary. This rewrites every invocation of the alias in a repository's tracked
// text files to `rustright`: `rightkit cargo|rustc|rustdoc|run|build|owns|status|...`, `["rightkit", "cargo", ...]`,
// `run("rightkit", ...)`, `cmd: "rightkit"`. Rerun it after a rebase instead of resolving conflicts by hand.
// It never touches history or byte-pinned content (RETIRED_ALIAS_EXEMPT in scripts/preflight.mjs, the same list the
// retired-alias rule exempts), nor a generated file (AGENTS.md): their sources are swept, then regenerated. Delete this file once every repository passes that rule.
//
// Usage: node scripts/retire-rightkit-alias.mjs [--repo <dir>] [--dry-run]
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { RETIRED_ALIAS_EXEMPT, isGeneratedFile, retireAliasText } from "./preflight.mjs";

const arg = (flag) => { const i = process.argv.indexOf(flag); return i >= 0 ? process.argv[i + 1] : undefined; };
const repo = path.resolve(arg("--repo") ?? path.join(path.dirname(fileURLToPath(import.meta.url)), ".."));
const dryRun = process.argv.includes("--dry-run");
const files = execFileSync("git", ["ls-files", "-z"], { cwd: repo, encoding: "utf8", maxBuffer: 256 * 1024 * 1024 }).split("\0").filter(Boolean);
let changedFiles = 0, changedRefs = 0;
for (const file of files) {
  if (RETIRED_ALIAS_EXEMPT.test(file)) continue;
  const full = path.join(repo, file);
  let bytes;
  try { if (!fs.lstatSync(full).isFile()) continue; bytes = fs.readFileSync(full); } catch { continue; }
  if (bytes.includes(0)) continue; // binary
  const text = bytes.toString("utf8");
  if (isGeneratedFile(text)) continue; // edit its sources instead (AGENTS.md: docs/agent-rules/*.md, then manage.py sync)
  const { text: next, count } = retireAliasText(text);
  if (!count) continue;
  changedFiles += 1; changedRefs += count;
  if (!dryRun) fs.writeFileSync(full, next);
  console.log(`${dryRun ? "would rewrite" : "rewrote"} ${count} in ${file}`);
}
console.log(`${repo}: ${changedRefs} invocation(s) in ${changedFiles} file(s) ${dryRun ? "would move" : "moved"} to rustright`);
