#!/usr/bin/env node
// Syntax gate: `node --check` every Git-listed .mjs/.js/.cjs file outside node_modules,
// so a file that cannot parse never reaches a test run or a publish.
import { execFileSync, spawnSync } from "node:child_process";

const listed = execFileSync("git", ["ls-files", "-z", "--", "*.mjs", "*.cjs", "*.js"], { encoding: "utf8", maxBuffer: 64 * 1024 * 1024 })
  .split("\0")
  .filter((file) => file && !file.split("/").includes("node_modules") && !file.includes("/fixtures/"));

const failures = [];
for (const file of listed) {
  const result = spawnSync(process.execPath, ["--check", file], { encoding: "utf8" });
  if (result.status !== 0) failures.push(`${file}\n${(result.stderr || "").trim()}`);
}

if (failures.length) {
  console.error(`syntax check failed for ${failures.length} of ${listed.length} file(s):\n\n${failures.join("\n\n")}`);
  process.exit(1);
}
console.log(`syntax check passed: ${listed.length} file(s)`);
