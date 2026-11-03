#!/usr/bin/env node
// `right-release publish-local npm --package @rightkit/<name> --commit <sha>`: the local npm route (Adrian, 2026-11-03, rightkit#1471:
// "i want you to publish it from local. the npm and crates are both local. we will do a provenance skip fuck your ci. feel free to push
// but no ci"). Mac only: the token is the granular @rightkit token with 2FA bypass in the Keychain item npm-rightkit-publish, read inside
// this process, never printed, never on a command line, never left on disk.
//
// It publishes exactly the bytes of one commit, and only after:
//   1. the commit is on origin/main (pushed; [skip ci] is fine);
//   2. the gate's Class A for the package passes on that commit (`rightkit-gate --to check --unit npm:<name> --commit <sha>`, this
//      package's own gate, so a release publishes through the gate it ships);
//   3. the version is not on npm yet.
// The commit's whole tree is exported (git archive), its dependencies are installed there offline from the commit's own lockfile and the
// checkout's pnpm store (a package whose prepack builds needs its devDependencies, rightkit#1493), and the package is packed there; the
// checkout and the gate's snapshot are never written. npm provenance is off (rightkit#1472). After publishing, the registry's shasum must
// equal the packed tarball's, or the command fails.
import crypto from "node:crypto";
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { NPM_KEYCHAIN_SERVICE, readKeychainToken } from "./publication-gate.mjs";

const REGISTRY = "https://registry.npmjs.org/";
const GATE = fileURLToPath(new URL("./bin/rightkit-gate.mjs", import.meta.url));

function fail(message) { throw new Error(message); }

/** The pnpm store the checkout installed from (`storeDir` in its node_modules/.modules.yaml, e.g. /Volumes/D/.pnpm-store/v11), as a `--store-dir` value (its parent). */
export function checkoutStoreDir(repo, readFile = (f) => fs.readFileSync(f, "utf8")) {
  let text;
  try { text = readFile(path.join(repo, "node_modules", ".modules.yaml")); } catch { fail(`${repo}/node_modules/.modules.yaml is missing: install the checkout once so its pnpm store is known`); }
  const m = /"?storeDir"?\s*:\s*"?([^"\r\n]+)"?/.exec(text);
  if (!m) fail(`${repo}/node_modules/.modules.yaml names no storeDir`);
  const storeDir = m[1].trim();
  return /^v\d+$/.test(path.basename(storeDir)) ? path.dirname(storeDir) : storeDir;
}

export function parseArgs(argv) {
  const out = { kind: argv[0], pkg: null, commit: null, repo: process.cwd() };
  for (let i = 1; i < argv.length; i += 1) {
    if (argv[i] === "--package") out.pkg = argv[++i];
    else if (argv[i] === "--commit") out.commit = argv[++i];
    else if (argv[i] === "--repo") out.repo = path.resolve(argv[++i]);
    else fail(`unknown option ${argv[i]}`);
  }
  if (out.kind !== "npm") fail("publish-local supports npm; crates publish with publish-local cargo once it lands");
  if (!/^@rightkit\/[a-z0-9-]+$/.test(out.pkg ?? "")) fail("--package @rightkit/<name> is required");
  if (!/^[0-9a-f]{7,40}$/.test(out.commit ?? "")) fail("--commit <sha> is required");
  return out;
}

export function publishLocalNpm(argv, { run = spawnSync, log = console.log, platform = process.platform, readToken = () => readKeychainToken(run) } = {}) {
  const o = parseArgs(argv);
  if (platform !== "darwin") fail(`publish-local npm runs on the Mac (the token is the Keychain item ${NPM_KEYCHAIN_SERVICE}); this host is ${platform}`);
  const git = (args) => {
    const r = run("git", args, { cwd: o.repo, encoding: "utf8", maxBuffer: 1 << 26 });
    if (r.status !== 0) fail(`git ${args.join(" ")} failed: ${String(r.stderr ?? "").trim()}`);
    return String(r.stdout ?? "").trim();
  };
  const commit = git(["rev-parse", "--verify", `${o.commit}^{commit}`]);
  git(["fetch", "-q", "origin", "main"]);
  if (run("git", ["merge-base", "--is-ancestor", commit, "origin/main"], { cwd: o.repo }).status !== 0) fail(`${commit} is not on origin/main: push it first ([skip ci])`);
  const dir = `packages/${o.pkg.slice("@rightkit/".length)}`;
  const manifest = JSON.parse(git(["show", `${commit}:${dir}/package.json`]));
  if (manifest.name !== o.pkg) fail(`${dir}/package.json at ${commit} names ${manifest.name}, not ${o.pkg}`);
  const version = manifest.version;

  const already = run("npm", ["view", `${o.pkg}@${version}`, "version", "--prefer-online", "--registry", REGISTRY], { encoding: "utf8" });
  if (String(already.stdout ?? "").trim() === version) fail(`${o.pkg}@${version} is already on npm; bump the version`);

  log(`publish-local: Class A for npm:${o.pkg} on ${commit.slice(0, 12)}`);
  const classA = run(process.execPath, [GATE, "--to", "check", "--unit", `npm:${o.pkg}`, "--commit", commit], { cwd: o.repo, stdio: ["ignore", "inherit", "inherit"] });
  if (classA.status !== 0) fail(`Class A failed for npm:${o.pkg} on ${commit} (exit ${classA.status}); nothing published`);

  // The agent volume, not the internal drive: the export holds a full install (workspace rule: run output under /Volumes/D/scratch).
  const scratchRoot = fs.existsSync("/Volumes/D/scratch") ? "/Volumes/D/scratch" : os.tmpdir();
  const work = fs.mkdtempSync(path.join(scratchRoot, "rk-publish-local-"));
  try {
    const tree = path.join(work, "tree");
    fs.mkdirSync(tree);
    const archive = run("git", ["archive", "--format=tar", commit], { cwd: o.repo, maxBuffer: 1 << 30 });
    if (archive.status !== 0) fail(`git archive ${commit} failed`);
    const tarIn = run("tar", ["-x", "-C", tree], { input: archive.stdout });
    if (tarIn.status !== 0) fail("unpacking the commit's tree failed");
    // The package and its workspace dependencies, from the commit's lockfile only, offline from the store; no dependency's install scripts run.
    const install = run("pnpm", ["install", "--frozen-lockfile", "--offline", "--ignore-scripts", "--ignore-pnpmfile", "--store-dir", checkoutStoreDir(o.repo), "--filter", `${o.pkg}...`], { cwd: tree, encoding: "utf8", maxBuffer: 1 << 28 });
    if (install.status !== 0) fail(`pnpm install (offline, frozen lockfile) for ${o.pkg} failed: ${String(install.stderr || install.stdout || "").trim().split(/\r?\n/).slice(-3).join(" | ")}`);
    // Workspace dependencies resolve through their dist/ (gitignored, so not in the export): build them first, in dependency order.
    const deps = run("pnpm", ["-r", "--filter", `${o.pkg}^...`, "run", "--if-present", "build"], { cwd: tree, encoding: "utf8", maxBuffer: 1 << 28 });
    if (deps.status !== 0) fail(`building ${o.pkg}'s workspace dependencies failed: ${String(deps.stderr || deps.stdout || "").trim().split(/\r?\n/).slice(-3).join(" | ")}`);
    const pkgDir = path.join(tree, dir);
    const packed = run("pnpm", ["pack", "--pack-destination", work], { cwd: pkgDir, encoding: "utf8" });
    if (packed.status !== 0) fail(`pnpm pack failed: ${String(packed.stderr ?? packed.stdout ?? "").trim().split(/\r?\n/).slice(-3).join(" | ")}`);
    const tgz = fs.readdirSync(work).find((f) => f.endsWith(".tgz"));
    if (!tgz) fail("pnpm pack wrote no tarball");
    const bytes = fs.readFileSync(path.join(work, tgz));
    const sha1 = crypto.createHash("sha1").update(bytes).digest("hex");
    log(`publish-local: packed ${tgz} sha1 ${sha1}`);

    const token = readToken();
    const userconfig = path.join(work, "npmrc");
    fs.writeFileSync(userconfig, "//registry.npmjs.org/:_authToken=${RIGHTKIT_NPM_TOKEN}\n", { mode: 0o600 });
    const redact = (text) => String(text ?? "").split(token).join("<redacted>");
    const pub = run("npm", ["publish", path.join(work, tgz), "--access", "public", "--provenance=false", "--registry", REGISTRY, "--userconfig", userconfig],
      { encoding: "utf8", env: { ...process.env, RIGHTKIT_NPM_TOKEN: token } });
    if (pub.status !== 0) fail(`npm publish failed:\n${redact(`${pub.stdout ?? ""}${pub.stderr ?? ""}`).trim()}`);

    // The registry may take a moment to serve a new version.
    let shasum = "";
    for (let i = 0; i < 20 && !shasum; i += 1) {
      const view = run("npm", ["view", `${o.pkg}@${version}`, "dist.shasum", "--prefer-online", "--registry", REGISTRY], { encoding: "utf8" });
      shasum = String(view.stdout ?? "").trim();
      if (!shasum) run("sleep", ["15"]);
    }
    if (shasum !== sha1) fail(`registry shasum ${shasum || "(not served yet)"} != packed ${sha1} for ${o.pkg}@${version}`);
    log(`publish-local: ${o.pkg}@${version} published from ${commit.slice(0, 12)}; registry shasum ${shasum} matches`);
    return { name: o.pkg, version, commit, shasum };
  } finally {
    fs.rmSync(work, { recursive: true, force: true });
  }
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try { publishLocalNpm(process.argv.slice(2)); } catch (e) { console.error(`right-release publish-local: ${e.message}`); process.exit(1); }
}
