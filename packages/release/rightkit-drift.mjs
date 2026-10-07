#!/usr/bin/env node
// RightKit adoption drift: which @rightkit/* and rightkit-* pins in an app are behind the latest
// published versions, and (with --apply) move them forward and refresh the lockfiles.
// Self-contained (node: built-ins only) so the SessionStart hook can carry a byte copy of it.
//   node rightkit-drift.mjs [--apply [--all]] [app-root]   (--all also adopts breaking bumps)
import { spawnSync } from "./background-process.mjs";
import { existsSync, readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const DEPENDENCY_SECTIONS = ["dependencies", "devDependencies", "optionalDependencies", "peerDependencies"];
const CARGO_PIN = /^(\s*)(rightkit-[A-Za-z0-9_-]+)(\s*=\s*(?:\{[^}\n]*?\bversion\s*=\s*)?")([=^~]?)(\d+\.\d+\.\d+)(")/;

export function loadRightkitVersions() {
  for (const name of ["rightkit-versions.json", "ownership-session-versions.json"]) {
    const file = path.join(here, name);
    if (existsSync(file)) return JSON.parse(readFileSync(file, "utf8"));
  }
  throw new Error("rightkit versions manifest not found next to rightkit-drift.mjs");
}

const numeric = (value) => String(value).split("-")[0].split(".").map((part) => Number.parseInt(part, 10) || 0);
const newer = (candidate, current) => { const a = numeric(candidate); const b = numeric(current); for (let i = 0; i < 3; i += 1) if (a[i] !== b[i]) return a[i] > b[i]; return false; };
/** Semver-breaking for 0.x crates/packages is a minor bump; from 1.0 it is a major bump. */
export const breakingBump = (from, to) => { const a = numeric(from); const b = numeric(to); return a[0] === 0 && b[0] === 0 ? a[1] !== b[1] : a[0] !== b[0]; };
const latestFor = (versions, ecosystem, name) => ecosystem === "npm" ? versions.stagedNpm?.[name] ?? versions.npm?.[name] : versions.stagedCargo?.[name] ?? versions.cargo?.[name];

/** Tracked package.json and Cargo.toml files, excluding RightKit's own source tree and vendored trees. */
export function dependencyManifests(appRoot) {
  const listed = spawnSync("git", ["-C", appRoot, "ls-files", "-z", "--", "package.json", "*/package.json", "Cargo.toml", "*/Cargo.toml"], { encoding: "utf8", windowsHide: true, maxBuffer: 16 * 1024 * 1024 });
  const files = listed.status === 0 ? listed.stdout.split("\0").filter(Boolean) : ["package.json", "Cargo.toml"].filter((file) => existsSync(path.join(appRoot, file)));
  return files.filter((file) => !/(^|\/)(node_modules|vendor|target|dist)\//.test(file) && !file.startsWith("tools/rightkit/"));
}

/** Pins behind the latest published RightKit version. Workspace, path, git and range-less specs are skipped. */
export function rightkitDrift(appRoot, versions = loadRightkitVersions()) {
  const drift = [];
  for (const file of dependencyManifests(appRoot)) {
    const text = readFileSync(path.join(appRoot, file), "utf8");
    if (file.endsWith("package.json")) {
      let manifest; try { manifest = JSON.parse(text); } catch { continue; }
      for (const section of DEPENDENCY_SECTIONS) {
        for (const [name, spec] of Object.entries(manifest[section] ?? {})) {
          const match = name.startsWith("@rightkit/") && typeof spec === "string" ? spec.match(/^([\^~=]?)(\d+\.\d+\.\d+)$/) : null;
          const latest = match && latestFor(versions, "npm", name);
          if (latest && newer(latest, match[2])) drift.push({ ecosystem: "npm", name, file, current: match[2], latest, breaking: breakingBump(match[2], latest) });
        }
      }
      continue;
    }
    for (const line of text.split(/\r?\n/)) {
      if (/\bpath\s*=/.test(line)) continue;
      const match = line.match(CARGO_PIN);
      const latest = match && latestFor(versions, "cargo", match[2]);
      if (latest && newer(latest, match[5])) drift.push({ ecosystem: "cargo", name: match[2], file, current: match[5], latest, breaking: breakingBump(match[5], latest) });
    }
  }
  return drift;
}

function run(command, args, cwd) {
  const result = spawnSync(command, args, { cwd, stdio: "inherit", windowsHide: true, shell: process.platform === "win32" });
  if (result.status !== 0) throw new Error(`${command} ${args.join(" ")} failed in ${cwd} (exit ${result.status ?? result.error?.message})`);
}

/** Move drifted pins to the latest version (keeping their = ^ ~ prefix), then refresh lockfiles.
 * Breaking bumps need code changes, so they are adopted only with includeBreaking. */
export function applyRightkitAdoption(appRoot, versions = loadRightkitVersions(), { refreshLocks = true, includeBreaking = false } = {}) {
  const drift = rightkitDrift(appRoot, versions).filter((item) => includeBreaking || !item.breaking);
  const byFile = Map.groupBy(drift, (item) => item.file);
  for (const [file, items] of byFile) {
    const full = path.join(appRoot, file); let text = readFileSync(full, "utf8");
    if (file.endsWith("package.json")) {
      const manifest = JSON.parse(text);
      for (const item of items) for (const section of DEPENDENCY_SECTIONS) {
        const spec = manifest[section]?.[item.name];
        if (typeof spec === "string") manifest[section][item.name] = spec.replace(item.current, item.latest);
      }
      text = `${JSON.stringify(manifest, null, 2)}\n`;
    } else {
      const latest = new Map(items.map((item) => [item.name, item.latest]));
      text = text.split(/(?<=\n)/).map((line) => line.replace(CARGO_PIN, (all, indent, name, mid, prefix, version, close) => /\bpath\s*=/.test(line) || !latest.has(name) ? all : `${indent}${name}${mid}${prefix}${latest.get(name)}${close}`)).join("");
    }
    writeFileSync(full, text);
  }
  if (refreshLocks) {
    for (const file of byFile.keys()) {
      const dir = path.join(appRoot, path.dirname(file));
      if (file.endsWith("package.json")) run("pnpm", ["install", "--lockfile-only", "--ignore-scripts"], dir);
      else run("rightkit", ["cargo", "metadata", "--format-version", "1", "--manifest-path", path.join(appRoot, file)], dir);
    }
  }
  return drift;
}

export function formatDrift(drift) {
  if (!drift.length) return "RightKit pins are current.";
  return [`RightKit updates available (${drift.length}):`, ...drift.map((d) => `- ${d.name} ${d.current} -> ${d.latest}${d.breaking ? " (breaking)" : ""} (${d.file})`)].join("\n");
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const args = process.argv.slice(2); const apply = args.includes("--apply");
  const root = path.resolve(args.find((arg) => !arg.startsWith("--")) ?? process.cwd());
  try {
    const drift = apply ? applyRightkitAdoption(root, undefined, { includeBreaking: args.includes("--all") }) : rightkitDrift(root);
    process.stdout.write(`${apply && drift.length ? `Adopted:\n${formatDrift(drift).split("\n").slice(1).join("\n")}` : formatDrift(drift)}\n`);
  } catch (error) { process.stderr.write(`rightkit-drift: ${error.message}\n`); process.exitCode = 1; }
}
