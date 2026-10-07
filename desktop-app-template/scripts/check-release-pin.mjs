// Version-agnostic release-pin check. Keep in the app. No version literal lives here:
//  1. package.json declares @rightkit/release as an exact devDependency version,
//  2. the installed node_modules/@rightkit/release is that exact version,
//  3. that version is in the installed release's rightkit-versions.json (npm, stagedNpm or legacyNpm).
import { existsSync, readFileSync } from "node:fs";
import { createRequire } from "node:module";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const NAME = "@rightkit/release";
const problems = [];
const readJson = (file) => JSON.parse(readFileSync(file, "utf8"));

const pkg = readJson(path.join(root, "package.json"));
const declared = pkg.devDependencies?.[NAME];
if (!declared) {
  problems.push(`${NAME} is not declared in devDependencies`);
} else if (!/^\d+\.\d+\.\d+$/.test(declared)) {
  problems.push(`${NAME} must be an exact version (no range/tag/protocol), got "${declared}"`);
}

const installedDir = path.join(root, "node_modules", ...NAME.split("/"));
let installed = null;
if (!existsSync(path.join(installedDir, "package.json"))) {
  problems.push(`${NAME} is not installed (run pnpm install)`);
} else {
  installed = readJson(path.join(installedDir, "package.json")).version;
  if (declared && installed !== declared) problems.push(`installed ${NAME}@${installed} != declared ${declared}`);
  const versionsFile = path.join(installedDir, "rightkit-versions.json");
  if (!existsSync(versionsFile)) {
    problems.push(`installed ${NAME} has no rightkit-versions.json`);
  } else {
    const manifest = readJson(versionsFile);
    const listed =
      manifest.npm?.[NAME] === installed ||
      manifest.stagedNpm?.[NAME] === installed ||
      (manifest.legacyNpm?.[NAME] ?? []).includes(installed);
    if (!listed) problems.push(`${NAME}@${installed} is not listed in the installed rightkit-versions.json (npm, stagedNpm or legacyNpm)`);
  }
}

if (problems.length) {
  for (const p of problems) console.error(`FAIL ${p}`);
  process.exit(1);
}
console.log(`${NAME}@${installed} pinned, installed and listed`);
