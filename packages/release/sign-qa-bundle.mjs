#!/usr/bin/env node
// right-release sign-qa-bundle: QA-ONLY ad hoc signing of an already-assembled .app that lives inside a QA scratch root.
// Why it exists: an assembled QA bundle (for example an Accessibility QA copy) needs a coherent ad hoc signature, but
// `codesign --force --deep --sign -` routed through the signing agent (SSH or non-Aqua session) is refused by design
// (macos-signing-session.mjs codesignAgentItems; rightkit-signing-agent.mjs). Those refusals stay. This module never calls the
// agent: it spawns /usr/bin/codesign itself, signs nested code one item at a time, deepest first, the outer bundle last, with
// identity "-" and never `--deep`. Ad hoc output is not a release artifact: it is never notarized, sealed, published or installed
// under /Applications, and the receipt says so (signature "adhoc", releaseArtifact false, qaOnly true).
import { existsSync, lstatSync, mkdtempSync, readdirSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { pathToFileURL } from "node:url";
import { spawnSync } from "./background-process.mjs";
import { adhocSafeEntitlements } from "./adhoc-entitlements.mjs";

export const QA_BUNDLE_SIGNING_SCHEMA = 1;
export const CODESIGN = "/usr/bin/codesign";
const PLUTIL = "/usr/bin/plutil";
// macOS paths only: posix semantics keep the checks identical on every host that unit-tests this module.
const px = path.posix;

export class QaBundleSigningError extends Error {
  constructor(code, message, details = {}) {
    super(message);
    this.name = "QaBundleSigningError";
    this.code = code;
    Object.assign(this, details);
  }
}
const fail = (code, message, details) => { throw new QaBundleSigningError(code, message, details); };

const defaultIo = { lstatSync, readdirSync, realpathSync, existsSync, mkdtempSync, writeFileSync, rmSync };
const captureOptions = { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"], windowsHide: true, shell: false };
const outputOf = (result) => `${result.stdout ?? ""}\n${result.stderr ?? ""}`.trim();
const inside = (parent, child) => child !== parent && child.startsWith(parent === "/" ? "/" : `${parent}/`);

function checked(run, args, code, command = CODESIGN) {
  const result = run(command, args, captureOptions);
  if (!result || result.error || result.status !== 0) {
    fail(code, `${command} ${args[0]} failed (${result?.status ?? "unavailable"}): ${result?.error?.message || outputOf(result ?? {})}`, { status: result?.status });
  }
  return result;
}

function lstatOrNull(io, file) { try { return io.lstatSync(file); } catch { return null; } }

function requireAbsolute(label, value) {
  if (typeof value !== "string" || !value.startsWith("/")) fail("QA_SIGN_PATH_NOT_ABSOLUTE", `${label} must be an absolute macOS path: ${JSON.stringify(value)}`);
  if (value.split("/").includes("..") || value.includes("\0")) fail("QA_SIGN_PATH_NOT_NORMAL", `${label} must not contain ".." segments: ${JSON.stringify(value)}`);
  return value.length > 1 ? value.replace(/\/+$/, "") : value;
}

/** Validate the target and return its real paths. Refusals are typed and happen before any codesign call. */
export function resolveQaBundleTarget({ app, qaScratchRoot, platform = process.platform, home = os.homedir(), io = defaultIo }) {
  if (platform !== "darwin") fail("QA_SIGN_DARWIN_ONLY", `sign-qa-bundle runs on macOS only (platform ${platform})`);
  const lexicalApp = requireAbsolute("--app", app);
  const lexicalRoot = requireAbsolute("--qa-root", qaScratchRoot);
  const lexicalHome = requireAbsolute("home", home);
  const applications = ["/Applications", px.join(lexicalHome, "Applications")];
  const real = (label, file) => { try { return io.realpathSync(file); } catch { return fail("QA_SIGN_PATH_MISSING", `${label} does not exist: ${file}`); } };
  const realApplications = applications.flatMap((dir) => { const out = [dir]; try { out.push(io.realpathSync(dir)); } catch { /* absent */ } return out; });
  const inApplications = (file) => realApplications.some((dir) => file === dir || inside(dir, file));
  if (inApplications(lexicalApp)) fail("QA_SIGN_APPLICATIONS_REFUSED", `QA signing refuses installed apps (/Applications, ~/Applications): ${lexicalApp}`);
  if (px.extname(lexicalApp) !== ".app") fail("QA_SIGN_NOT_APP", `target is not a .app bundle: ${lexicalApp}`);
  const link = lstatOrNull(io, lexicalApp);
  if (!link) fail("QA_SIGN_PATH_MISSING", `--app does not exist: ${lexicalApp}`);
  if (link.isSymbolicLink()) fail("QA_SIGN_LINK_REFUSED", `--app is a symbolic link; pass the real bundle: ${lexicalApp}`);
  if (!link.isDirectory()) fail("QA_SIGN_NOT_APP", `target is not a directory bundle: ${lexicalApp}`);
  const realRoot = real("--qa-root", lexicalRoot);
  const realApp = real("--app", lexicalApp);
  if (realRoot === "/" || realRoot === lexicalHome || inApplications(realRoot) || realApplications.some((dir) => inside(realRoot, dir))) {
    fail("QA_SIGN_ROOT_REFUSED", `QA scratch root is too broad or an installed-app location: ${realRoot}`);
  }
  if (inApplications(realApp)) fail("QA_SIGN_APPLICATIONS_REFUSED", `QA signing refuses installed apps (resolved to ${realApp})`);
  if (!inside(realRoot, realApp)) fail("QA_SIGN_OUTSIDE_QA_ROOT", `bundle ${realApp} is not inside the QA scratch root ${realRoot}`);
  if (px.extname(realApp) !== ".app") fail("QA_SIGN_NOT_APP", `target resolves to a non-.app path: ${realApp}`);
  return { app: lexicalApp, realApp, realRoot };
}

/** Bundle kinds that are signed as a unit, and the directory whose files are loose Mach-O candidates. */
const BUNDLE_EXTENSIONS = new Set([".app", ".xpc", ".framework"]);

function readInfo(run, bundle, cache, io) {
  if (cache.has(bundle)) return cache.get(bundle);
  let info = {};
  for (const location of ["Contents/Info.plist", "Resources/Info.plist", "Info.plist"]) {
    const file = px.join(bundle, location);
    if (!io.existsSync(file)) continue;
    const result = checked(run, ["-convert", "json", "-o", "-", file], "QA_SIGN_PLIST_INVALID", PLUTIL);
    try { info = JSON.parse(result.stdout); } catch { fail("QA_SIGN_PLIST_INVALID", `Invalid Info.plist JSON: ${file}`); }
    break;
  }
  cache.set(bundle, info);
  return info;
}

/**
 * Nested code inside the bundle, ordered inside-out: deepest path first (stable by path), the outer bundle excluded.
 * Symbolic links are never entered or signed; a link that resolves outside the bundle is refused.
 */
export function enumerateNestedCode({ realApp, run, io = defaultIo }) {
  const infoCache = new Map();
  const found = new Map();
  const add = (file, kind) => { if (!found.has(file)) found.set(file, kind); };
  const bundleOf = (dir) => px.dirname(px.dirname(dir)); // .../X.app/Contents/MacOS -> .../X.app
  const walk = (dir) => {
    for (const entry of io.readdirSync(dir, { withFileTypes: true })) {
      const file = px.join(dir, entry.name);
      if (entry.isSymbolicLink()) {
        let target;
        try { target = io.realpathSync(file); } catch { fail("QA_SIGN_LINK_REFUSED", `dangling symbolic link inside the bundle: ${file}`); }
        if (target !== realApp && !inside(realApp, target)) fail("QA_SIGN_LINK_REFUSED", `symbolic link escapes the bundle: ${file} -> ${target}`);
        continue;
      }
      const ext = px.extname(entry.name);
      if (entry.isDirectory()) {
        if (BUNDLE_EXTENSIONS.has(ext)) add(file, ext === ".framework" ? "framework" : ext === ".xpc" ? "xpc" : "app");
        else if (px.basename(dir) === "LoginItems" && px.basename(px.dirname(dir)) === "Library") add(file, "login-item");
        walk(file);
      } else if (entry.isFile()) {
        if (ext === ".dylib") add(file, "dylib");
        else if (px.basename(dir) === "MacOS" && px.basename(px.dirname(dir)) === "Contents") {
          const main = readInfo(run, bundleOf(dir), infoCache, io).CFBundleExecutable;
          if (typeof main !== "string" || !main) fail("QA_SIGN_PLIST_INVALID", `no CFBundleExecutable for ${bundleOf(dir)}`);
          if (entry.name !== main) add(file, "executable");
        } else if (px.basename(dir) === "Frameworks" && px.basename(px.dirname(dir)) === "Contents") add(file, "executable");
        else if (px.basename(dir) === "LoginItems" && px.basename(px.dirname(dir)) === "Library") add(file, "login-item");
      }
    }
  };
  walk(realApp);
  const depth = (file) => file.split("/").length;
  return [...found].map(([file, kind]) => ({ path: file, kind }))
    .sort((a, b) => depth(b.path) - depth(a.path) || (a.path < b.path ? -1 : 1));
}

/** The only codesign argv this module emits. `--deep` and any identity other than "-" are structurally impossible. */
export function adhocCodesignArgs(item, entitlementsFile) {
  const args = ["--force", "--sign", "-"];
  if (entitlementsFile) args.push("--entitlements", entitlementsFile);
  args.push(item);
  if (args.includes("--deep") || args[args.indexOf("--sign") + 1] !== "-") fail("QA_SIGN_ARGV_REFUSED", "internal: ad hoc argv must never carry --deep or a real identity");
  return args;
}

export function signQaBundleAdhoc({ app, qaScratchRoot, run = spawnSync, platform = process.platform, home = os.homedir(), entitlements, io = defaultIo, out } = {}) {
  const target = resolveQaBundleTarget({ app, qaScratchRoot, platform, home, io });
  const { realApp, realRoot } = target;
  let scratch;
  try {
    let outerEntitlements;
    let entitlementKeys = [];
    if (entitlements !== undefined) {
      const source = requireAbsolute("entitlements", entitlements);
      const parsed = checked(run, ["-convert", "json", "-o", "-", source], "QA_SIGN_ENTITLEMENTS_INVALID", PLUTIL);
      let reduced;
      try { reduced = adhocSafeEntitlements(JSON.parse(parsed.stdout)); } catch { fail("QA_SIGN_ENTITLEMENTS_INVALID", `Invalid entitlements plist: ${source}`); }
      entitlementKeys = Object.keys(reduced).sort();
      if (entitlementKeys.length) {
        scratch = io.mkdtempSync(px.join(realRoot, ".sign-qa-"));
        outerEntitlements = px.join(scratch, "adhoc-entitlements.plist");
        io.writeFileSync(outerEntitlements, JSON.stringify(reduced));
        checked(run, ["-convert", "xml1", outerEntitlements], "QA_SIGN_ENTITLEMENTS_INVALID", PLUTIL);
      }
    }
    const nested = enumerateNestedCode({ realApp, run, io });
    const plan = [...nested, { path: realApp, kind: "bundle" }];
    const signed = [];
    for (const item of plan) {
      const withEntitlements = item.kind === "bundle" ? outerEntitlements : undefined;
      checked(run, adhocCodesignArgs(item.path, withEntitlements), "QA_SIGN_CODESIGN_FAILED");
      signed.push({ path: item.path, kind: item.kind });
    }
    checked(run, ["--verify", "--strict", realApp], "QA_SIGN_VERIFY_FAILED");
    const receipt = {
      schema: QA_BUNDLE_SIGNING_SCHEMA, signature: "adhoc", releaseArtifact: false, qaOnly: true,
      notarized: false, published: false, usedSigningAgent: false,
      app: realApp, qaScratchRoot: realRoot, entitlementKeys, verified: "codesign --verify --strict", signed,
    };
    if (out) {
      const file = requireAbsolute("--out", out);
      io.writeFileSync(file, `${JSON.stringify(receipt, null, 2)}\n`);
    }
    return receipt;
  } finally {
    if (scratch) { try { io.rmSync(scratch, { recursive: true, force: true }); } catch { /* best effort */ } }
  }
}

export function main(args = process.argv.slice(2), options = {}) {
  const take = (flag) => {
    const index = args.indexOf(flag);
    if (index < 0) return undefined;
    const value = args[index + 1];
    if (!value || value.startsWith("--")) fail("QA_SIGN_USAGE", `${flag} requires a value`);
    return value;
  };
  try {
    for (const arg of args.filter((a) => a.startsWith("--"))) {
      if (!["--app", "--qa-root", "--out", "--entitlements"].includes(arg)) fail("QA_SIGN_USAGE", `unknown option ${arg}`);
    }
    const receipt = signQaBundleAdhoc({ app: take("--app"), qaScratchRoot: take("--qa-root"), entitlements: take("--entitlements"), out: take("--out"), ...options });
    console.log(`ad hoc signed (QA only, not a release artifact) ${receipt.signed.length} item(s), outer bundle last: ${receipt.app}`);
  } catch (error) {
    console.error(`right-release sign-qa-bundle: ${error.code ? `[${error.code}] ` : ""}${error.message}`);
    process.exitCode = 1;
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) main();
