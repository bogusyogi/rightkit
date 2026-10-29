// Trusted, data-only view of a separate candidate checkout. Never load candidate code.
import fs from "node:fs";
import path from "node:path";
import crypto from "node:crypto";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { parseYaml } from "./workflow-yaml.mjs";
import { derivePublicationObligations } from "./publication-obligations.mjs";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const SHA = /^[a-f0-9]{40}$/;
const LIMIT = 8 * 1024 * 1024;
const inside = (parent, child) => { const relative = path.relative(parent, child); return relative === "" || (!relative.startsWith(`..${path.sep}`) && relative !== ".." && !path.isAbsolute(relative)); };
const digest = (bytes) => crypto.createHash("sha256").update(bytes).digest("hex");
const object = (value, label) => {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error(`invalid ${label}`);
  return value;
};
const safePath = (name) => typeof name === "string" && name.length <= 1024 && !/[\\\x00-\x1f\x7f:]/.test(name) && name.split("/").every((part) => part && part !== "." && part !== ".." && !/[. ]$/.test(part));
const ALLOW_FIELDS = { hostExclusion: "allowHostExclusion", noTest: "allowNoTest", ignored: "allowIgnored", missingAssets: "allowMissingAssets" };

function obligationAllowsets(allow, units) {
  object(allow, "candidate allowsets");
  if (![Object.prototype, null].includes(Object.getPrototypeOf(allow))) throw new Error("invalid candidate allowsets object");
  if (Reflect.ownKeys(allow).some((key) => !Object.hasOwn(ALLOW_FIELDS, key))) throw new Error("unknown candidate allowset");
  return Object.fromEntries(Object.entries(ALLOW_FIELDS).map(([input, output]) => {
    const names = Object.hasOwn(allow, input) ? allow[input] : [];
    if (!Array.isArray(names) || [...names].some((name) => typeof name !== "string" || !name || /\s/.test(name)) || new Set(names).size !== names.length || !Array.isArray(units) || names.some((name) => !units.includes(name))) throw new Error(`invalid candidate allowset ${input}: expected unique requested-unit names`);
    return [output, [...names]];
  }));
}

/** sourceSha/promotionSha must come from the trusted caller's validated committed request. */
export function loadCandidateSource({ directory, sourceSha, promotionSha, registry, units, allow = {}, spawn = spawnSync } = {}) {
  if (!SHA.test(sourceSha ?? "") || !SHA.test(promotionSha ?? "")) throw new Error("invalid candidate or promotion SHA");
  const normalizedAllow = obligationAllowsets(allow, units);
  if (typeof directory !== "string" || !path.isAbsolute(directory)) throw new Error("candidate directory must be absolute");
  const root = fs.realpathSync(directory);
  const trusted = fs.realpathSync(ROOT);
  if (inside(trusted, root) || inside(root, trusted)) throw new Error("candidate must be separate from trusted checkout");
  // Reject symlink/junction aliases even if their final target happens to remain in scope.
  for (let next = path.resolve(directory);;) {
    if (fs.lstatSync(next).isSymbolicLink()) throw new Error("candidate path contains a symlink");
    const parent = path.dirname(next); if (parent === next) break; next = parent;
  }
  if (!fs.statSync(root).isDirectory()) throw new Error("candidate is not a directory");
  const gitDirectory = fs.lstatSync(path.join(root, ".git"));
  if (!gitDirectory.isDirectory() || gitDirectory.isSymbolicLink()) throw new Error("candidate requires its own Git directory");
  // status may run clean filters. A checkout created by the trusted checkout action needs
  // no executable filter/diff config or included/worktree config. Inspect before invoking Git.
  const localConfig = path.join(root, ".git", "config");
  const configStat = fs.lstatSync(localConfig);
  if (!configStat.isFile() || configStat.isSymbolicLink() || configStat.size > 256 * 1024) throw new Error("unsafe candidate Git config");
  const configText = fs.readFileSync(localConfig, "utf8");
  if (/^\s*\[\s*(?:include(?:if)?|filter|diff)(?:\s|\]|\.)/im.test(configText) || /^\s*worktreeConfig\s*=/im.test(configText)) throw new Error("candidate Git config may execute or include external configuration");
  const env = Object.fromEntries(Object.entries(process.env).filter(([key]) => !/^GIT_/i.test(key)));
  Object.assign(env, { GIT_CONFIG_NOSYSTEM: "1", GIT_CONFIG_GLOBAL: process.platform === "win32" ? "NUL" : "/dev/null", GIT_NO_REPLACE_OBJECTS: "1", GIT_TERMINAL_PROMPT: "0", GIT_OPTIONAL_LOCKS: "0" });
  function git(args, { absent = false, maxBuffer = LIMIT } = {}) {
    const result = spawn("git", ["-c", "core.hooksPath=", "-c", "core.fsmonitor=false", "-c", "core.untrackedCache=false", ...args], { cwd: root, env, encoding: "buffer", maxBuffer, timeout: 30_000, windowsHide: true, shell: false, stdio: ["ignore", "pipe", "pipe"] });
    if (result.error || result.signal || result.status !== 0 && !(absent && result.status === 1)) throw new Error(`candidate git ${args[0]} failed`);
    const stdout = Buffer.from(result.stdout ?? "");
    if (stdout.length > maxBuffer || Buffer.byteLength(result.stderr ?? "") > maxBuffer) throw new Error("candidate git output exceeds limit");
    return { status: result.status, bytes: stdout, text: stdout.toString("utf8") };
  }
  const text = (args) => git(args).text.trim();
  if (fs.realpathSync(text(["rev-parse", "--show-toplevel"])) !== root) throw new Error("candidate is not checkout root");
  if (text(["rev-parse", "--verify", "HEAD^{commit}"]) !== sourceSha) throw new Error("candidate HEAD differs from source SHA");
  if (text(["rev-parse", "--is-shallow-repository"]) !== "false") throw new Error("candidate requires full history");
  if (git(["merge-base", "--is-ancestor", sourceSha, promotionSha], { absent: true }).status !== 0) throw new Error("candidate is not an ancestor of promotion");
  const tree = text(["rev-parse", "--verify", `${sourceSha}^{tree}`]);
  if (!SHA.test(tree)) throw new Error("invalid candidate tree");
  if (text(["status", "--porcelain=v1", "--untracked-files=all", "--ignore-submodules=none"]) || git(["ls-files", "--others", "-z"]).bytes.length) throw new Error("candidate has tracked drift or untracked files");
  const flags = git(["ls-files", "-v", "-z"]).text.split("\0").filter(Boolean);
  if (flags.some((row) => !row.startsWith("H "))) throw new Error("candidate index contains hidden or unmerged entries");
  const entries = new Map();
  const rows = git(["ls-tree", "-r", "-z", "--full-tree", sourceSha]).text.split("\0").filter(Boolean);
  if (rows.length > 100_000) throw new Error("candidate tree exceeds entry limit");
  for (const row of rows) {
    const match = /^(100644|100755) blob ([a-f0-9]{40})\t(.+)$/.exec(row);
    if (!match || !safePath(match[3])) throw new Error("candidate tree has unsafe path, symlink or submodule");
    const name = match[3];
    if (name.split("/").some((part) => ["node_modules", ".git"].includes(part.toLowerCase()))) throw new Error("candidate contains dependency installation or nested Git data");
    const full = path.join(root, ...name.split("/"));
    for (let next = full; next !== root; next = path.dirname(next)) {
      if (fs.lstatSync(next).isSymbolicLink()) throw new Error("candidate tree contains symlink escape");
    }
    if (!fs.statSync(full).isFile()) throw new Error("candidate tracked path is not a file");
    entries.set(name, match[2]);
  }
  const sourceHashes = {};
  let total = 0;
  function readData(name) {
    const blob = entries.get(name);
    if (!blob) throw new Error(`candidate missing ${name}`);
    const size = text(["cat-file", "-s", blob]);
    if (!/^\d+$/.test(size) || Number(size) > LIMIT || (total += Number(size)) > 32 * LIMIT) throw new Error("candidate source data exceeds size limit");
    const bytes = git(["cat-file", "blob", blob]).bytes;
    if (bytes.length !== Number(size) || crypto.createHash("sha1").update(`blob ${bytes.length}\0`).update(bytes).digest("hex") !== blob) throw new Error("candidate blob authentication failed");
    sourceHashes[name] = digest(bytes);
    return Buffer.from(bytes);
  }
  const read = (name) => readData(name).toString("utf8");
  const json = (name) => object(JSON.parse(read(name)), name);
  const config = json("ci/gate.config.json");
  const assetGated = json("ci/asset-gated.json");
  read("package.json"); read("pnpm-lock.yaml");
  const workspaceText = read("pnpm-workspace.yaml");
  const workspace = parseYaml(workspaceText);
  // Match gate.mjs exactly, but refuse policy constructs its regex would silently ignore.
  if (!Array.isArray(workspace.packages) || workspace.packages[0] !== "packages/*" || workspace.packages.slice(1).some((item) => typeof item !== "string" || !/^!packages\/[^/\s]+$/.test(item))) throw new Error("unsupported candidate workspace package policy");
  const excluded = new Set([...workspaceText.matchAll(/^\s*-\s*['"]?!packages\/([^'"\s/]+)['"]?\s*$/gm)].map((match) => match[1]));
  if (excluded.size !== new Set(workspace.packages.slice(1)).size) throw new Error("workspace exclusions disagree with gate parser");
  // rightkit#1285: every crates/<x> is its own Cargo workspace with its own Cargo.lock; there is no root manifest or lock.
  const memberManifests = []; const packageManifests = []; const packageNames = new Map(); const crateLocks = {};
  for (const name of [...entries.keys()].sort()) {
    if (/^crates\/[^/]+\/Cargo.toml$/.test(name)) {
      const manifest = read(name);
      if (/^\[package\][^[]*?^name\s*=\s*"([^"]+)"/ms.test(manifest)) {
        const dir = path.posix.dirname(name);
        memberManifests.push({ dir, text: manifest });
        crateLocks[dir] = read(`${dir}/Cargo.lock`);
      }
    }
    if (/^packages\/[^/]+\/package.json$/.test(name)) {
      const manifest = json(name);
      // The gate ignores nameless package manifests, while authenticating their bytes still matters.
      if (manifest.name) {
        packageNames.set(path.posix.dirname(name), manifest.name);
        if (!excluded.has(name.split("/")[1])) packageManifests.push({ dir: path.posix.dirname(name), json: manifest });
      }
    }
  }
  if (!memberManifests.length || !packageManifests.length) throw new Error("candidate inventory is empty");
  // Authenticate declarations from committed source without running a candidate scanner.
  const hits = git(["grep", "-I", "-l", "-z", "-e", "RIGHTKIT_REQUIRE_ASSETS", sourceSha, "--", "crates", "packages"], { absent: true }).text.split("\0").filter(Boolean);
  const provisionedNpm = new Set(["@rightkit/qa", "@rightkit/qa-chrome"]);
  for (const hit of hits) {
    const name = hit.slice(sourceSha.length + 1);
    if (!hit.startsWith(`${sourceSha}:`) || !entries.has(name)) throw new Error("invalid candidate asset scan output");
    if (!/\.(rs|mjs|ts|tsx|js)$/.test(name) || name.split("/").some((part) => ["node_modules", "target", "dist", ".git"].includes(part))) continue;
    const dir = name.split("/").slice(0, 2).join("/");
    const member = memberManifests.find((item) => item.dir === dir);
    const unit = member ? /^\[package\][^[]*?^name\s*=\s*"([^"]+)"/ms.exec(member.text)?.[1] : packageNames.get(dir);
    if (unit && !Object.hasOwn(assetGated[member ? "crates" : "npm"] ?? {}, unit) && !(name.startsWith("packages/") && provisionedNpm.has(unit))) throw new Error(`candidate asset declaration missing for ${unit}`);
  }
  const data = { registry, units, allow: normalizedAllow, config, assetGated, crateLocks, memberManifests, packageManifests };
  const hosts = Object.fromEntries(["macos", "windows"].map((host) => [host, derivePublicationObligations({ ...data, host })]));
  // Recheck mutable checkout identity after reading data (all semantic bytes came from blobs).
  if (text(["rev-parse", "--verify", "HEAD^{commit}"]) !== sourceSha || text(["status", "--porcelain=v1", "--untracked-files=all", "--ignore-submodules=none"])) throw new Error("candidate changed during source loading");
  return { source: { sha: sourceSha, tree }, hosts, root, sourceHashes, readData };
}
