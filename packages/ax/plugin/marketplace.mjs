// One marketplace for the whole Right suite. The source of truth is a list of
// plugin sources (git URL + path + ref + version); this module generates each
// host's marketplace manifest from it and validates that every listed plugin
// resolves and passes `plugin validate`.
//
// Host formats (evidenced in pulse/ and legion/):
//   Claude  .claude-plugin/marketplace.json
//           { "$schema"?, "name", "owner": { "name" }, "plugins": [ { "name", "source", "description" } ] }
//           Pulse and Legion use a relative-path source ("./plugins/claude/pulse", "./").
//   Codex   .agents/plugins/marketplace.json
//           { "name", "plugins": [ { "name", "source": { "source": "local", "path" },
//                                    "policy": { "installation", "authentication" }, "category" } ] }
//           Only the `local` source is evidenced, so a Codex entry is generated
//           only for a plugin vendored inside the marketplace repository
//           (`hosts.codex.local`).
//
// A plugin vendored inside the marketplace repository itself is listed with
// `hosts.<host>.local: "./plugins/<name>"` (a ./relative path inside that
// repository). Claude then gets the relative-path source STRING
// ("./plugins/<name>", as Pulse and Legion write it), Codex the
// { source: "local", path } object, both evidenced by files in this workspace.
//
// A suite marketplace can also list plugins that live in other repositories, which the
// relative-path source cannot express. For those the Claude entry uses the
// documented remote source objects { source: "url", url, ref } (plugin at the
// repository root) and { source: "git-subdir", url, path, ref } (plugin in a
// subdirectory). Those two shapes are NOT evidenced by a file in this
// workspace; they come from Claude Code's marketplace documentation. See
// README "Suite plugins".

import { execFileSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import { basename, dirname, join, resolve, sep } from "node:path";
import { resolvesOutsideRoot } from "../lib/path-safety.mjs";
import { detectLayout, validateRepoLayout } from "./validate.mjs";

export const SOURCE_LIST_SCHEMA_VERSION = 1;
export const CLAUDE_MARKETPLACE_PATH = ".claude-plugin/marketplace.json";
export const CODEX_MARKETPLACE_PATH = ".agents/plugins/marketplace.json";
export const CLAUDE_MARKETPLACE_SCHEMA = "https://json.schemastore.org/claude-code-plugin-marketplace.json";
export const HOSTS = Object.freeze(["claude", "codex"]);

const KEBAB = /^[a-z0-9]+(?:-[a-z0-9]+)*$/;
const SEMVER = /^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$/;
const HTTPS_URL = /^https:\/\/[A-Za-z0-9.-]+(?::\d+)?\/[^\s?#]+$/;
const FILE_URL = /^file:\/\/\/[^\s?#]+$/;

function finding(ruleId, severity, claim, evidence, remediation) {
  return { ruleId, severity, claim, evidence, remediation };
}

function isPlainObject(value) {
  return Boolean(value) && typeof value === "object" && !Array.isArray(value);
}

function validRelativePath(value) {
  if (typeof value !== "string" || !value) return false;
  if (value === ".") return true;
  if (value.includes("\\") || value.startsWith("/") || /^[A-Za-z]:/.test(value)) return false;
  return value.split("/").every((part) => part && part !== "." && part !== "..");
}

function validRelativeDotPath(value) {
  return typeof value === "string" && value.startsWith("./") && validRelativePath(value.slice(2));
}

// Validates the source list. Returns { list, findings }; list is null when the
// shape is too broken to build from.
export function parseSourceList(input, { allowFileUrls = false } = {}) {
  const findings = [];
  const bad = (claim, evidence, remediation) => findings.push(finding("marketplace-sources", "error", claim, evidence, remediation));
  if (!isPlainObject(input)) {
    bad("plugin source list must be a JSON object", typeof input, "start from marketplace/plugins.json");
    return { list: null, findings };
  }
  if (input.schemaVersion !== SOURCE_LIST_SCHEMA_VERSION) bad(`schemaVersion must be ${SOURCE_LIST_SCHEMA_VERSION}`, String(input.schemaVersion), "set schemaVersion: 1");
  if (typeof input.name !== "string" || !KEBAB.test(input.name)) bad("marketplace name must be lowercase kebab-case", JSON.stringify(input.name), "for example \"right-suite\"");
  if (!isPlainObject(input.owner) || typeof input.owner.name !== "string" || !input.owner.name.trim()) bad("owner.name is required", JSON.stringify(input.owner), "set owner: { \"name\": \"Orthic Labs\" }");
  if (!Array.isArray(input.plugins)) {
    bad("plugins must be an array", typeof input.plugins, "list one entry per plugin that exists");
    return { list: null, findings };
  }
  const seen = new Set();
  input.plugins.forEach((entry, index) => {
    const at = `plugins[${index}]`;
    if (!isPlainObject(entry)) { bad(`${at} must be an object`, typeof entry, "describe the plugin source"); return; }
    if (typeof entry.name !== "string" || !KEBAB.test(entry.name)) bad(`${at}.name must be lowercase kebab-case`, JSON.stringify(entry.name), "use the plugin's own name");
    else if (seen.has(entry.name)) bad(`${at}.name is listed twice`, entry.name, "one entry per plugin name");
    else seen.add(entry.name);
    if (typeof entry.description !== "string" || entry.description.trim().length < 20 || /[\r\n]/.test(entry.description)) bad(`${at}.description must be one line of at least 20 characters`, JSON.stringify(entry.description), "say what the plugin lets an agent do");
    // url and ref describe the plugin's own repository; an entry whose every host is `local` has none.
    const remoteNeeded = !isPlainObject(entry.hosts) || Object.values(entry.hosts).some((spec) => !(isPlainObject(spec) && spec.local !== undefined));
    if (remoteNeeded || entry.url !== undefined) {
      const urlOk = typeof entry.url === "string" && (HTTPS_URL.test(entry.url) || (allowFileUrls && FILE_URL.test(entry.url)));
      if (!urlOk) bad(`${at}.url must be a public https git URL`, JSON.stringify(entry.url), "for example https://github.com/Orthic-Labs/pulse.git");
      else if (/^https:\/\/[^/]*@/.test(entry.url)) bad(`${at}.url must not carry credentials`, entry.url.replace(/\/\/[^@/]*@/, "//***@"), "remove the userinfo from the URL");
    }
    if (remoteNeeded || entry.ref !== undefined) {
      if (typeof entry.ref !== "string" || !entry.ref || /\s/.test(entry.ref)) bad(`${at}.ref must name a branch or tag`, JSON.stringify(entry.ref), "for example \"main\" or \"v0.1.0\"");
    }
    if (entry.sha !== undefined && !(typeof entry.sha === "string" && /^[0-9a-f]{40}$/.test(entry.sha))) bad(`${at}.sha must be a 40-character lowercase hex commit`, JSON.stringify(entry.sha), "pin a full commit or remove sha");
    if (entry.version !== undefined && !(typeof entry.version === "string" && SEMVER.test(entry.version))) bad(`${at}.version must be semver`, JSON.stringify(entry.version), "for example 0.1.0");
    if (entry.knownIssues !== undefined && (typeof entry.knownIssues !== "string" || entry.knownIssues.trim().length < 20)) bad(`${at}.knownIssues must explain, in at least 20 characters, why plugin validate errors are waived`, JSON.stringify(entry.knownIssues), "name the owner and the fix, or remove the waiver");
    if (entry.category !== undefined && (typeof entry.category !== "string" || !entry.category.trim())) bad(`${at}.category must be a non-empty string`, JSON.stringify(entry.category), "for example \"productivity\"");
    if (!isPlainObject(entry.hosts) || Object.keys(entry.hosts).length === 0) {
      bad(`${at}.hosts needs at least one host`, JSON.stringify(entry.hosts), `use { "claude": { "path": "." } } or { "claude": { "local": "./plugins/name" } }; hosts: ${HOSTS.join(", ")}`);
      return;
    }
    for (const [host, spec] of Object.entries(entry.hosts)) {
      if (!HOSTS.includes(host)) { bad(`${at}.hosts.${host} is not a known host`, host, `known hosts: ${HOSTS.join(", ")}`); continue; }
      if (!isPlainObject(spec)) { bad(`${at}.hosts.${host} must be an object`, typeof spec, `use { "path": "." } or { "local": "./plugins/name" }`); continue; }
      if (spec.local !== undefined) {
        // Vendored in the marketplace repository: refuse absolute paths, backslashes and `..`.
        if (!validRelativeDotPath(spec.local)) bad(`${at}.hosts.${host}.local must be a ./relative path inside the marketplace repository`, JSON.stringify(spec.local), "for example ./plugins/pulse; no absolute path, no .., no backslashes");
        if (spec.path !== undefined && !validRelativePath(spec.path)) bad(`${at}.hosts.${host}.path must be a relative POSIX path inside the plugin repository ("." for the root)`, JSON.stringify(spec.path), "no leading /, no .., no backslashes");
        continue;
      }
      if (!validRelativePath(spec.path)) bad(`${at}.hosts.${host}.path must be a relative POSIX path inside the plugin repository ("." for the root)`, JSON.stringify(spec?.path), "no leading /, no .., no backslashes");
    }
    if (isPlainObject(entry.hosts) && !entry.hosts.claude) {
      findings.push(finding("marketplace-sources", "warning", `${at} has no claude host, so it is absent from the Claude marketplace`, entry.name, "add hosts.claude"));
    }
  });
  const fatal = findings.some((item) => item.severity === "error");
  return { list: fatal ? null : input, findings };
}

export function buildClaudeMarketplace(list) {
  const plugins = list.plugins.filter((entry) => entry.hosts.claude).map((entry) => {
    const spec = entry.hosts.claude;
    let source;
    if (spec.local) source = spec.local;
    else {
      source = spec.path === "."
        ? { source: "url", url: entry.url, ref: entry.ref }
        : { source: "git-subdir", url: entry.url, path: spec.path, ref: entry.ref };
      if (entry.sha) source.sha = entry.sha;
    }
    const out = { name: entry.name, source, description: entry.description };
    if (entry.version) out.version = entry.version;
    if (entry.category) out.category = entry.category;
    return out;
  });
  return { $schema: CLAUDE_MARKETPLACE_SCHEMA, name: list.name, owner: { name: list.owner.name }, plugins };
}

// Codex: only plugins vendored in this repository (hosts.codex.local) can be
// listed, because only the `local` source shape is evidenced.
export function buildCodexMarketplace(list) {
  const plugins = [];
  const skipped = [];
  for (const entry of list.plugins) {
    const spec = entry.hosts.codex;
    if (!spec) continue;
    if (!spec.local) { skipped.push(entry.name); continue; }
    plugins.push({
      name: entry.name,
      source: { source: "local", path: spec.local },
      policy: { installation: "AVAILABLE", authentication: "ON_INSTALL" },
      ...(entry.category ? { category: entry.category } : {}),
    });
  }
  return { manifest: { name: list.name, plugins }, skipped };
}

export function renderMarketplaceFiles(list) {
  const files = [{ path: CLAUDE_MARKETPLACE_PATH, content: `${JSON.stringify(buildClaudeMarketplace(list), null, 2)}\n` }];
  const codex = buildCodexMarketplace(list);
  if (codex.manifest.plugins.length) files.push({ path: CODEX_MARKETPLACE_PATH, content: `${JSON.stringify(codex.manifest, null, 2)}\n` });
  return { files, codexSkipped: codex.skipped };
}

export function readSourceList(sourcesPath, options) {
  let parsed;
  try { parsed = JSON.parse(readFileSync(sourcesPath, "utf8")); } catch (error) {
    return { list: null, findings: [finding("marketplace-sources", "error", "plugin source list is missing or not valid JSON", `${sourcesPath}: ${String(error?.message ?? error)}`, "create marketplace/plugins.json")] };
  }
  return parseSourceList(parsed, options);
}

// Writes the generated manifests under outDir, or (check) reports drift.
export function buildMarketplace({ sourcesPath, outDir, check = false, allowFileUrls = false }) {
  const { list, findings } = readSourceList(sourcesPath, { allowFileUrls });
  if (!list) return { ok: false, written: [], drift: [], findings };
  const { files, codexSkipped } = renderMarketplaceFiles(list);
  const written = [];
  const drift = [];
  for (const file of files) {
    const target = join(outDir, ...file.path.split("/"));
    const current = existsSync(target) ? readFileSync(target, "utf8") : null;
    if (check) {
      if (current !== file.content) drift.push(file.path);
      continue;
    }
    mkdirSync(dirname(target), { recursive: true });
    writeFileSync(target, file.content, "utf8");
    written.push(target);
  }
  const notes = [...findings];
  for (const name of codexSkipped) notes.push(finding("marketplace-codex-remote", "info", "plugin has a codex host but no hosts.codex.local, so it is not in the Codex marketplace (only the local source shape is evidenced)", name, "ship the plugin's own .agents/plugins/marketplace.json, or vendor it and set hosts.codex.local"));
  if (check) for (const path of drift) notes.push(finding("marketplace-drift", "error", "generated marketplace manifest is stale or missing", path, "run `right-ax marketplace build`"));
  return { ok: !notes.some((item) => item.severity === "error"), written, drift, findings: notes };
}

function repoDirName(url) {
  return basename(url.replace(/\/+$/, "")).replace(/\.git$/, "");
}

function gitClone(entry, parentDir) {
  const dest = join(parentDir, repoDirName(entry.url));
  const env = { ...process.env, GIT_TERMINAL_PROMPT: "0" };
  const run = (args) => execFileSync("git", args, { env, stdio: ["ignore", "pipe", "pipe"], timeout: 180000 });
  if (entry.sha) {
    run(["clone", "--quiet", entry.url, dest]);
    run(["-C", dest, "checkout", "--quiet", entry.sha]);
  } else {
    run(["clone", "--quiet", "--depth", "1", "--branch", entry.ref, entry.url, dest]);
  }
  return dest;
}

// A --workspace that is the marketplace repository itself (the source list lives inside it) rather than a
// directory of checkouts.
export function workspaceIsMarketplaceRoot(workspace, sourcesPath) {
  if (!workspace || !sourcesPath) return false;
  const root = resolve(workspace);
  const sources = resolve(sourcesPath);
  return sources.startsWith(root + sep);
}

// Resolves where an entry's plugin root for `host` lives on this machine.
//   local:     a plugin vendored in the marketplace repository (`hosts.<host>.local`). Resolved inside the
//              marketplace root: --workspace when that IS the marketplace repository, else --out (where
//              the manifests are generated, so the same root the host resolves relative sources against).
//              No clone, no checkout of another repository; symlinks that leave the root are refused.
//   workspace: a directory holding one checkout per repository, named after the repository
//              (<workspace>/pulse for .../pulse.git), or the marketplace repository root itself, in which
//              case its own name matches its own entry and other repositories are looked up beside it
//              (<workspace>/../pulse). Validates the checked-out working tree, not necessarily `ref`.
//   fetch:     shallow-clones `ref` (or checks out `sha`) into a temp directory.
export function resolvePluginRoot(entry, host, { workspace, fetch, cloneCache, tempParent, marketplaceRoot, sourcesPath }) {
  const spec = entry.hosts[host];
  if (spec.local) {
    const root = workspaceIsMarketplaceRoot(workspace, sourcesPath) ? resolve(workspace) : marketplaceRoot ? resolve(marketplaceRoot) : null;
    if (!root) throw new Error("cannot resolve a local path: pass --out <marketplace repository root> or --workspace <marketplace repository root>");
    if (!existsSync(root)) throw new Error(`no marketplace repository at ${root}`);
    if (resolvesOutsideRoot(spec.local, root)) throw new Error(`local path ${spec.local} escapes the marketplace repository`);
    const pluginRoot = join(root, ...spec.local.slice(2).split("/"));
    if (!existsSync(pluginRoot)) throw new Error(`local path ${spec.local} does not exist in the marketplace repository ${root}`);
    return { pluginRoot, source: `local ${spec.local} in marketplace repository ${root}` };
  }
  let repoRoot;
  let source;
  if (fetch) {
    const key = `${entry.url}#${entry.sha ?? entry.ref}`;
    if (!cloneCache.has(key)) cloneCache.set(key, gitClone(entry, tempParent));
    repoRoot = cloneCache.get(key);
    source = `cloned ${entry.url} @ ${entry.sha ?? entry.ref}`;
  } else if (workspace) {
    const base = resolve(workspace);
    const name = repoDirName(entry.url);
    const candidates = workspaceIsMarketplaceRoot(workspace, sourcesPath)
      ? [basename(base) === name ? base : null, join(base, name), join(dirname(base), name)]
      : [join(base, name)];
    repoRoot = candidates.find((candidate) => candidate && existsSync(candidate)) ?? candidates.find(Boolean);
    source = `workspace checkout ${repoRoot} (working tree, not ref ${entry.ref})`;
    if (!existsSync(repoRoot)) throw new Error(`no checkout at ${repoRoot}`);
  } else {
    throw new Error("cannot resolve: pass --workspace <dir of checkouts> or --fetch");
  }
  if (resolvesOutsideRoot(spec.path, repoRoot)) throw new Error(`path ${spec.path} escapes the repository`);
  const pluginRoot = spec.path === "." ? repoRoot : join(repoRoot, ...spec.path.split("/"));
  if (!existsSync(pluginRoot)) throw new Error(`path ${spec.path} does not exist in ${source}`);
  return { pluginRoot, source };
}

function manifestIdentity(pluginRoot) {
  for (const rel of ["plugin.json", ".claude-plugin/plugin.json", ".codex-plugin/plugin.json"]) {
    const path = join(pluginRoot, ...rel.split("/"));
    if (!existsSync(path)) continue;
    try { return JSON.parse(readFileSync(path, "utf8")); } catch { return null; }
  }
  return null;
}

// Full check: source list valid, generated manifests in sync, every listed
// plugin resolves and passes `plugin validate` (description gate included),
// its manifest name equals the entry name and its version equals the entry's.
export function validateMarketplace({ sourcesPath, outDir, workspace, fetch = false, allowFileUrls = false, skipDrift = false }) {
  const built = skipDrift || !outDir
    ? { findings: [] }
    : buildMarketplace({ sourcesPath, outDir, check: true, allowFileUrls });
  const { list, findings: listFindings } = readSourceList(sourcesPath, { allowFileUrls });
  const findings = [...(built.findings ?? [])];
  if (!list) return { valid: false, plugins: [], findings: dedupe([...listFindings, ...findings]) };
  const plugins = [];
  const cloneCache = new Map();
  const tempParent = fetch ? mkdtempSync(join(os.tmpdir(), "right-ax-marketplace-")) : null;
  try {
    for (const entry of list.plugins) {
      for (const host of Object.keys(entry.hosts)) {
        const record = { plugin: entry.name, host, valid: false, source: null, findings: [] };
        plugins.push(record);
        try {
          const { pluginRoot, source } = resolvePluginRoot(entry, host, { workspace, fetch, cloneCache, tempParent, marketplaceRoot: outDir, sourcesPath });
          record.source = source;
          const result = validateRepoLayout(pluginRoot);
          record.findings.push(...result.findings.filter((item) => item.severity !== "info"));
          if (result.layout === "none") record.findings.push(finding("marketplace-plugin-layout", "error", "no plugin manifest found at the listed path", `${entry.name}/${host}: ${entry.hosts[host].local ?? entry.hosts[host].path}`, "point path at the directory holding plugin.json, .claude-plugin/ or .codex-plugin/"));
          const identity = manifestIdentity(pluginRoot);
          if (identity && identity.name !== entry.name) record.findings.push(finding("marketplace-plugin-name", "error", "plugin manifest name differs from the marketplace entry", `${identity.name} vs ${entry.name}`, "use the manifest's name in marketplace/plugins.json"));
          if (identity && entry.version && identity.version && identity.version !== entry.version) record.findings.push(finding("marketplace-plugin-version", "error", "plugin manifest version differs from the marketplace entry", `${identity.version} vs ${entry.version}`, "update the entry's version or release the plugin"));
          if (host === "claude" && detectLayout(pluginRoot) === "codex-native") record.findings.push(finding("marketplace-plugin-layout", "error", "claude host points at a Codex-only plugin root", entry.hosts[host].local ?? entry.hosts[host].path, "point hosts.claude.path at a root with plugin.json or .claude-plugin/plugin.json"));
        } catch (error) {
          record.findings.push(finding("marketplace-plugin-unresolved", "error", "listed plugin could not be resolved", `${entry.name}/${host}: ${String(error?.message ?? error)}`, "fix url/ref/path/local, or pass --workspace / --fetch"));
        }
        const errors = record.findings.filter((item) => item.severity === "error");
        const errorCount = errors.length;
        // Identity and resolution errors (marketplace-plugin-*) are never waivable.
        if (errorCount && entry.knownIssues && errors.every((item) => !item.ruleId.startsWith("marketplace-plugin-"))) {
          // A waiver never hides the errors: they stay in record.findings and the
          // waiver is itself reported, so the listing reads "known red", not "clean".
          record.waived = { errors: errorCount, reason: entry.knownIssues };
          findings.push(finding("marketplace-waived", "warning", `${entry.name}/${host}: ${errorCount} plugin validate error(s) waived by knownIssues`, entry.knownIssues, "fix the plugin and remove knownIssues"));
        }
        record.valid = errorCount === 0 || Boolean(record.waived);
        if (record.valid && entry.ref && !/^v?\d/.test(entry.ref) && !entry.sha) {
          findings.push(finding("marketplace-floating-ref", "info", "entry tracks a branch, so installs follow its head", `${entry.name}: ${entry.ref}`, "pin a tag or sha for reproducible installs"));
        }
      }
    }
  } finally {
    if (tempParent) rmSync(tempParent, { recursive: true, force: true });
  }
  const failed = plugins.some((record) => !record.valid);
  const all = dedupe([...listFindings, ...findings]);
  return { valid: !failed && !all.some((item) => item.severity === "error"), plugins, findings: all };
}

function dedupe(items) {
  const seen = new Set();
  return items.filter((item) => {
    const key = `${item.ruleId}|${item.evidence}`;
    if (seen.has(key)) return false;
    seen.add(key);
    return true;
  });
}
