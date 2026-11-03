#!/usr/bin/env node
// Plugin delivery with the app (rightkit#1442): the app's Claude plugin is packaged in the app, installed and updated by the same install transaction, and
// read back, so no plugin ever depends on a hand copy into a catalog or a dev checkout.
//
//   packaging   `targets.<platform>.plugin: { dir: "plugins/<app>" }` (release.mjs): the plugin must be valid and its version EQUAL the release version. The
//               plugin folder ships inside the app (bundle resources / payload); this check is what keeps the two from drifting.
//   install     `right-release install-app ... --plugin-dir <installed app's plugin folder>` and `right-release plugin-install` call registerAppPlugin after the
//               app itself is installed: a local marketplace `<app>-installed` is generated in the user's data folder from the INSTALLED plugin folder, added to
//               every Claude config root (the default one, CLAUDE_CONFIG_DIR, and every profile under `~/ClaudeProfiles`), the plugin is installed or updated
//               there with Claude's own CLI, and the version Claude recorded (`plugins/installed_plugins.json`) is read back and must equal the app's.
// Codex: no plugin install CLI contract is evidenced in this workspace, so Codex is NOT driven from here (the result reports it as not covered).
import { cpSync, existsSync, mkdirSync, readFileSync, readdirSync, renameSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { pathToFileURL } from "node:url";
import { spawnSync } from "./background-process.mjs";

export class PluginDeliveryError extends Error {
  constructor(message) { super(message); this.name = "PluginDeliveryError"; this.code = "RK_PLUGIN_DELIVERY"; }
}
const fail = (message) => { throw new PluginDeliveryError(message); };
const NAME = /^[a-z][a-z0-9-]{1,40}$/;
const VERSION = /^\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)?$/;

/** `<dir>/.claude-plugin/plugin.json` -> { name, version, description }. Throws when it is missing, unreadable, or its name or version is not valid. */
export function readPluginManifest(dir, { fs = { existsSync, readFileSync } } = {}) {
  const file = path.join(dir, ".claude-plugin", "plugin.json");
  if (!fs.existsSync(file)) fail(`${dir} is not a Claude plugin: ${file} is missing (right-ax plugin init)`);
  let manifest;
  try { manifest = JSON.parse(String(fs.readFileSync(file, "utf8"))); } catch (error) { fail(`${file} is not JSON: ${error.message}`); }
  if (typeof manifest?.name !== "string" || !NAME.test(manifest.name)) fail(`${file} needs a lowercase plugin name`);
  if (typeof manifest.version !== "string" || !VERSION.test(manifest.version)) fail(`${file} needs a semver version`);
  return { name: manifest.name, version: manifest.version, description: typeof manifest.description === "string" ? manifest.description : manifest.name };
}

/** The release-gate check: the plugin at `root/<dir>` is valid and carries exactly the app's version. */
export function assertPluginMatchesApp({ root, plugin, version, fs }) {
  if (plugin === null || typeof plugin !== "object" || Array.isArray(plugin) || typeof plugin.dir !== "string" || Object.keys(plugin).some((key) => key !== "dir")) fail("targets.<platform>.plugin must be { dir: \"<app-relative plugin folder>\" }");
  if (path.isAbsolute(plugin.dir) || path.win32.isAbsolute(plugin.dir) || plugin.dir.split(/[\\/]/).includes("..")) fail(`plugin.dir must be a relative path inside the app: ${plugin.dir}`);
  const manifest = readPluginManifest(path.resolve(root, plugin.dir), { fs });
  if (manifest.version !== version) fail(`the plugin ${manifest.name} is ${manifest.version} but the app is ${version}: they ship as one release, so the versions must be equal`);
  return manifest;
}

/** Claude config roots to register in: explicit `roots`, else `~/.claude`, `CLAUDE_CONFIG_DIR`, and the `.claude` folder of each profile under `~/ClaudeProfiles`. Existing directories only, de-duplicated. */
export function claudeConfigRoots({ home = os.userInfo().homedir, env = process.env, roots, fs = { existsSync, readdirSync } } = {}) {
  const candidates = roots ?? [path.join(home, ".claude"), ...(env.CLAUDE_CONFIG_DIR ? [env.CLAUDE_CONFIG_DIR] : [])];
  if (!roots) {
    const profiles = path.join(home, "ClaudeProfiles");
    if (fs.existsSync(profiles)) for (const name of fs.readdirSync(profiles)) candidates.push(path.join(profiles, name, ".claude"));
  }
  const seen = new Set();
  return candidates.map((root) => path.resolve(root)).filter((root) => fs.existsSync(root) && !seen.has(root) && seen.add(root));
}

/** The marketplace folder for an app, in the user's data folder (never inside the signed app). */
export function marketplaceDir({ app, home = os.userInfo().homedir, platform = process.platform, env = process.env }) {
  if (!NAME.test(app)) fail(`app ${JSON.stringify(app)} is not an app slug`);
  const base = platform === "win32" ? path.join(env.LOCALAPPDATA || path.join(home, "AppData", "Local"), "RightKit")
    : platform === "darwin" ? path.join(home, "Library", "Application Support", "RightKit")
      : path.join(env.XDG_DATA_HOME || path.join(home, ".local", "share"), "rightkit");
  return path.join(base, "plugin-marketplaces", app);
}

/** Rebuild the local marketplace from the installed plugin folder: a copy of the plugin and its `.claude-plugin/marketplace.json`, swapped in by rename. */
export function writeLocalMarketplace({ app, pluginDir, dir, fs = { cpSync, mkdirSync, renameSync, rmSync, writeFileSync, existsSync, readFileSync } }) {
  const manifest = readPluginManifest(pluginDir, { fs });
  const name = `${app}-installed`;
  const staging = `${dir}.new`;
  fs.rmSync(staging, { recursive: true, force: true });
  fs.mkdirSync(path.join(staging, ".claude-plugin"), { recursive: true });
  fs.cpSync(pluginDir, path.join(staging, "plugins", manifest.name), { recursive: true, dereference: false, verbatimSymlinks: true });
  fs.writeFileSync(path.join(staging, ".claude-plugin", "marketplace.json"), `${JSON.stringify({ name, owner: { name: "Orthic Labs" }, plugins: [{ name: manifest.name, source: `./plugins/${manifest.name}`, version: manifest.version, description: manifest.description }] }, null, 2)}\n`);
  fs.mkdirSync(path.dirname(dir), { recursive: true });
  fs.rmSync(dir, { recursive: true, force: true });
  fs.renameSync(staging, dir);
  return { marketplace: name, plugin: manifest.name, version: manifest.version, dir };
}

/** The version Claude recorded for `plugin@marketplace` in one config root (user scope first), or null. */
export function installedPluginVersion({ root, plugin, marketplace, fs = { readFileSync } }) {
  let recorded;
  try { recorded = JSON.parse(String(fs.readFileSync(path.join(root, "plugins", "installed_plugins.json"), "utf8"))); } catch { return null; }
  const entries = recorded?.plugins?.[`${plugin}@${marketplace}`];
  if (!Array.isArray(entries) || !entries.length) return null;
  return (entries.find((entry) => entry?.scope === "user") ?? entries[0])?.version ?? null;
}

/**
 * Register the app's plugin in every Claude config root and read the version back. `pluginDir` is the INSTALLED app's plugin folder; `version` the app's.
 * Returns { roots: [{ root, version }], codex: "not covered" }. Throws PluginDeliveryError listing every root that did not end at `version`.
 * Injectable: run, fs, roots, home, env, claude (the CLI path).
 */
export function registerAppPlugin({ app, pluginDir, version, roots, claude = "claude", home = os.userInfo().homedir, env = process.env, platform = process.platform, run = spawnSync, fs, log = (line) => console.log(line) }) {
  const local = fs ? { fs } : {};
  const market = writeLocalMarketplace({ app, pluginDir, dir: marketplaceDir({ app, home, platform, env }), ...local });
  if (market.version !== version) fail(`the installed plugin is ${market.version}, the app is ${version}; the plugin folder in the app is stale`);
  const targets = claudeConfigRoots({ home, env, ...(roots ? { roots } : {}), ...(fs ? { fs } : {}) });
  if (!targets.length) fail("no Claude config root exists to register the plugin in");
  const ref = `${market.plugin}@${market.marketplace}`;
  const problems = [];
  const results = [];
  for (const root of targets) {
    // HOME from the password database and this root as the config dir: the registration lands in the root named, never in the invoking shell's profile.
    const childEnv = { ...env, HOME: home, CLAUDE_CONFIG_DIR: root };
    const step = (args) => run(claude, args, { env: childEnv, encoding: "utf8", windowsHide: true, shell: false });
    const added = step(["plugin", "marketplace", "add", market.dir]);
    if ((added?.error || added?.status !== 0) && !/already/i.test(`${added?.stderr}${added?.stdout}`)) { problems.push(`${root}: claude plugin marketplace add failed (${added?.error?.message ?? `exit ${added?.status}`})`); continue; }
    step(["plugin", "marketplace", "update", market.marketplace]);
    const installed = step(["plugin", "install", ref]);
    if ((installed?.error || installed?.status !== 0) && !/already/i.test(`${installed?.stderr}${installed?.stdout}`)) { problems.push(`${root}: claude plugin install ${ref} failed (${installed?.error?.message ?? `exit ${installed?.status}`})`); continue; }
    step(["plugin", "update", ref]);
    const got = installedPluginVersion({ root, plugin: market.plugin, marketplace: market.marketplace, ...(fs ? { fs } : {}) });
    if (got !== version) { problems.push(`${root}: Claude recorded ${got ?? "no install"} for ${ref}, expected ${version}`); continue; }
    results.push({ root, version: got });
    log(`right-release plugin: ${ref} ${got} registered in ${root}`);
  }
  if (problems.length) fail(`plugin delivery did not finish:\n${problems.map((line) => `- ${line}`).join("\n")}`);
  return { marketplace: market.marketplace, plugin: market.plugin, version, roots: results, codex: "not covered: no Codex plugin install contract is evidenced" };
}

export function main(argv = process.argv.slice(2)) {
  const opts = { app: null, pluginDir: null, version: null, roots: [], claude: "claude" };
  const flags = { "--app": "app", "--plugin-dir": "pluginDir", "--version": "version", "--claude-root": "roots", "--claude": "claude" };
  for (let i = 0; i < argv.length; i += 1) {
    const key = flags[argv[i]];
    if (!key || argv[i + 1] === undefined) { console.error(`right-release plugin-install: unknown or incomplete argument ${argv[i]}\nusage: right-release plugin-install --app <slug> --plugin-dir <installed plugin folder> [--version <v>] [--claude-root <dir>]... [--claude <cli>]`); return 2; }
    if (key === "roots") opts.roots.push(argv[i + 1]); else opts[key] = argv[i + 1];
    i += 1;
  }
  try {
    if (!opts.app || !opts.pluginDir) fail("--app and --plugin-dir are required");
    const pluginDir = path.resolve(opts.pluginDir);
    const version = opts.version ?? readPluginManifest(pluginDir).version;
    const result = registerAppPlugin({ app: opts.app, pluginDir, version, ...(opts.roots.length ? { roots: opts.roots } : {}), claude: opts.claude });
    console.log(JSON.stringify(result));
    return 0;
  } catch (error) { console.error(`right-release plugin-install: ${error.message}`); return error instanceof PluginDeliveryError ? 1 : 2; }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) process.exitCode = main();
