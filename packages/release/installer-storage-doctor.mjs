import { existsSync, lstatSync, readFileSync, readdirSync } from "node:fs";
import path from "node:path";
import { execFileSync } from "./background-process.mjs";
import { resolveManagedRoot } from "./artifact-storage.mjs";

const COPY_COMMAND = /\b(?:cp|copy|copy-item|rsync)\b/i;
const MIRROR_COMMAND = /\bmirror-root-artifact\b/i;
const INSTALLER_SOURCE = /(?:\$\{?[A-Z_]*(?:INSTALLER|SETUP|DMG|PKG|MSI|EXE|APP_ZIP|PACKAGE)[A-Z0-9_]*\}?|\b[^\s"']+\.(?:dmg|pkg|msi|exe|zip)\b)/i;
const REPO_DESTINATION = /(?:--package-root\s+|(?:^|\s))(?:["']?(?:\$\{?(?:REPO_ROOT|ROOT|REPOSITORY_ROOT)\}?)(?:\/[^"'\s]*)?|\.\.?\/?)["']?(?:\s|$)/i;
const MANAGED_DESTINATION = /(?:RIGHT_RELEASE_ARTIFACT_DIR|RIGHTKIT_MANAGED_ROOT|\.rightkit-managed|managedRoot|artifact(?:s)?\/)/i;
export const DEFAULT_STORAGE_THRESHOLD_BYTES = 5_000_000_000;

/** Report-only check for product scripts that copy installers into a checkout. */
export function findInstallerCopyScripts({ appRoot, workspaceRoot = "/Volumes/D/claude", knownPaths = ["scraperight/package.sh"], managedRoot = resolveManagedRoot() } = {}) {
  const findings = [];
  const seen = new Set();
  const add = (file, lineNumber, source, reason) => {
    const resolved = path.resolve(file);
    const key = `${resolved}:${lineNumber}`;
    if (seen.has(key)) return;
    seen.add(key);
    findings.push({ path: resolved, line: lineNumber, source: source.trim(), destination: destinationFor(source), reason });
  };
  const inspect = (file, label) => {
    let text;
    try { text = readFileSync(file, "utf8"); } catch { return; }
    const lines = text.split(/\r?\n/);
    for (let index = 0; index < lines.length; index += 1) {
      const source = lines[index].trim();
      if (!source || source.startsWith("#")) continue;
      if (isManagedDestination(source, managedRoot)) continue;
      if (isRepoInstallerCopy(source)) add(file, index + 1, source, `${label} copies installer into repository output path`);
    }
  };
  const packageFile = path.join(path.resolve(appRoot), "package.json");
  if (existsSync(packageFile)) {
    try {
      const packageJson = JSON.parse(readFileSync(packageFile, "utf8"));
      for (const [name, command] of Object.entries(packageJson.scripts ?? {})) {
        if (typeof command !== "string") continue;
        for (const [index, segment] of command.split(/\r?\n|&&|;/).entries()) {
          if (!isManagedDestination(segment, managedRoot) && isRepoInstallerCopy(segment)) add(packageFile, index + 1, segment, `script ${name} copies installer into repository output path`);
        }
      }
    } catch { /* malformed package.json is reported by existing doctor checks */ }
  }
  const root = path.resolve(appRoot);
  for (const entry of safeEntries(root)) {
    if (!entry.isFile() || !/\.(?:sh|bash|cmd|bat|ps1)$/i.test(entry.name)) continue;
    inspect(path.join(root, entry.name), "script");
  }
  for (const relative of knownPaths) {
    const file = path.join(path.resolve(workspaceRoot), relative);
    const resolved = path.resolve(file);
    if (existsSync(resolved) && (resolved === root || resolved.startsWith(`${root}${path.sep}`))) inspect(resolved, "known script");
  }
  return findings;
}

function isRepoInstallerCopy(line) {
  if (MIRROR_COMMAND.test(line)) return /--package-root\s+["']?(?:\$\{?(?:REPO_ROOT|ROOT|REPOSITORY_ROOT)\}?|\.\.?)/i.test(line);
  return COPY_COMMAND.test(line) && INSTALLER_SOURCE.test(line) && REPO_DESTINATION.test(line);
}

function isManagedDestination(line, managedRoot) {
  return MANAGED_DESTINATION.test(line) || (managedRoot && line.includes(path.resolve(managedRoot)));
}

function destinationFor(line) {
  const match = line.match(/(?:--package-root\s+|\s)(["']?(?:\$\{?[A-Z_][A-Z0-9_]*\}?|\.\.?)(?:\/[^"'\s]*)?\/?["']?)\s*$/i);
  return match?.[1] ?? null;
}

/** Find repository subdirectories above storage threshold outside managed root. */
export function findLargeRepositorySubdirectories({ appRoot, managedRoot = resolveManagedRoot(), thresholdBytes = DEFAULT_STORAGE_THRESHOLD_BYTES, ignored = gitIgnored } = {}) {
  if (!Number.isFinite(thresholdBytes) || thresholdBytes < 1) throw new TypeError("thresholdBytes must be positive");
  const root = path.resolve(appRoot);
  const ignoredRoot = managedRoot ? path.resolve(managedRoot) : null;
  const findings = [];
  const inside = (candidate, parent) => candidate === parent || candidate.startsWith(`${parent}${path.sep}`);
  const managedIgnored = ignoredRoot && inside(ignoredRoot, root) ? Boolean(ignored(ignoredRoot, root)) : false;
  const walk = (directory) => {
    let total = 0;
    let entries;
    try { entries = readdirSync(directory, { withFileTypes: true }); } catch { return 0; }
    for (const entry of entries) {
      if (entry.name === ".git") continue;
      const file = path.join(directory, entry.name);
      let stat;
      try { stat = lstatSync(file); } catch { continue; }
      if (stat.isSymbolicLink()) continue;
      if (stat.isDirectory()) {
        if (managedIgnored && inside(file, ignoredRoot)) continue;
        total += walk(file);
      } else if (stat.isFile()) total += stat.size;
    }
    if (total > thresholdBytes) findings.push({ path: directory, bytes: total, reason: "repository-directory-exceeds-5GB-outside-ignored-managed-path" });
    return total;
  };
  for (const entry of safeEntries(root)) {
    if (entry.name === ".git" || !entry.isDirectory() || entry.isSymbolicLink()) continue;
    const directory = path.join(root, entry.name);
    if (managedIgnored && inside(directory, ignoredRoot)) continue;
    walk(directory);
  }
  return findings;
}

function gitIgnored(candidate, root) {
  try {
    execFileSync("git", ["-C", root, "check-ignore", "--no-index", "-q", "--", path.relative(root, candidate)], { stdio: "ignore" });
    return true;
  } catch {
    return false;
  }
}

function safeEntries(directory) {
  try { return readdirSync(directory, { withFileTypes: true }); } catch { return []; }
}
