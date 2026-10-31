#!/usr/bin/env node
// `node windows-identity.mjs --config <file> [--json] [--dry-run]`: builds the app's Windows package-identity (sparse) MSIX from its release
// config's `windows.identity` (windows-identity-plan.mjs), the producer the app's own build-windows-identity.mjs used to be. Run by the gate
// inside the door's snapshot before the compile, on Windows only; under any other host it does nothing.
//
// Order (the archived script's, kept): stamp the manifest version into a staged copy; stage the asset PNGs into the package AND the external
// Assets dir (a sparse package resolves its logo pixels from the install location); render the scaled variants from the SVG; makepri builds
// resources.pri; makeappx packs with /nv (the identity package ships no executable by design); unpack round-trip; the PRI must index the
// manifest-backed assets; then the package is signed through sign-windows.mjs, the one provisioned Azure Artifact Signing path. This module
// never reads a credential, never authenticates, writes no signing receipt, and removes its scratch so only the package and external-assets remain.
import { copyFileSync, existsSync, mkdirSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { spawnSync } from "./background-process.mjs";
import { loadAppConfig } from "./command-spec.mjs";
import { findWindowsSdkTool } from "./preflight.mjs";
import { WORK_DIR, expectedPackageFiles, expectedPriUris, planWindowsIdentity, scaledNames, stampManifest, windowsIdentitySteps } from "./windows-identity-plan.mjs";
import { webBuildDoorRefusal } from "./web-build.mjs";

const SIGN_WINDOWS = path.join(path.dirname(fileURLToPath(import.meta.url)), "sign-windows.mjs");
const fail = (message) => { throw Object.assign(new Error(message), { code: "RK_WINDOWS_IDENTITY" }); };

/** The default SVG renderer: the app's own `@resvg/resvg-js` (resolved from the app root, never RightKit's). Returns a PNG Buffer. */
export function appResvgRenderer(appRoot) {
  const { Resvg } = createRequire(path.join(appRoot, "package.json"))("@resvg/resvg-js");
  return (svg, size) => new Resvg(svg, { background: "rgba(0,0,0,0)", fitTo: { mode: "width", value: size } }).render().asPng();
}

/**
 * Build the package. `deps` are injectable: `platform`, `run(cmd, args)`, `findTool`, `env`, `render(svg, size)`, `sign(file)`, `log`.
 * Returns `{ skipped, steps, out, package, files }` (`files` app-relative posix). Skips (no writes) when the package already exists.
 */
export function buildWindowsIdentity({ plan, appRoot }, deps = {}) {
  const { platform = process.platform, run = spawnSync, findTool = findWindowsSdkTool, env = process.env, render = null, log = (m) => console.error(`[windows-identity] ${m}`) } = deps;
  const at = (p) => path.resolve(appRoot, p);
  const outDir = at(plan.out);
  const msix = path.join(outDir, plan.package);
  const steps = windowsIdentitySteps(plan);
  if (platform !== "win32") return { skipped: "not-windows", steps, out: plan.out, package: plan.package, files: [] };
  if (existsSync(msix)) return { skipped: "present", steps, out: plan.out, package: plan.package, files: [] };
  const tool = (name, envName) => { const p = env[envName] || findTool(name)?.toolPath; if (!p || !existsSync(p)) fail(`${name} not found; install the Windows SDK or set ${envName}`); return p; };
  const makeappx = tool("makeappx.exe", "MAKEAPPX_PATH");
  const makepri = tool("makepri.exe", "MAKEPRI_PATH");
  const checked = (cmd, args, what) => { const r = run(cmd, args, { encoding: "utf8", windowsHide: true, timeout: 600000 }); if (r?.error || r?.status !== 0) fail(`${what} failed (${r?.error?.message ?? `exit ${r?.status}`}${r?.stderr ? `: ${String(r.stderr).trim()}` : ""})`); return r; };
  const work = path.join(outDir, WORK_DIR);
  const stage = path.join(work, "stage");
  const external = path.join(outDir, "external-assets", "Assets");
  try {
    rmSync(work, { recursive: true, force: true });
    mkdirSync(path.join(stage, "Assets"), { recursive: true });
    rmSync(path.join(outDir, "external-assets"), { recursive: true, force: true });
    mkdirSync(external, { recursive: true });
    const manifestSource = at(plan.manifest);
    if (!existsSync(manifestSource)) fail(`missing manifest ${plan.manifest}`);
    const stamped = stampManifest(readFileSync(manifestSource, "utf8"), plan.msixVersion);
    writeFileSync(path.join(stage, "AppxManifest.xml"), stamped.xml, "utf8");
    log(`staged manifest ${stamped.name} Version="${plan.msixVersion}"`);
    for (const f of plan.files) {
      if (!existsSync(at(f.from))) fail(`missing asset ${f.from}`);
      copyFileSync(at(f.from), path.join(stage, "Assets", f.to));
      copyFileSync(at(f.from), path.join(external, f.to));
    }
    if (plan.scaled) {
      if (!existsSync(at(plan.scaled.svg))) fail(`missing svg ${plan.scaled.svg}`);
      const svg = readFileSync(at(plan.scaled.svg), "utf8");
      const renderer = render ?? appResvgRenderer(appRoot);
      for (const size of plan.scaled.sizes) {
        const png = renderer(svg, size);
        const name = plan.scaled.name.replace("{size}", String(size));
        writeFileSync(path.join(stage, "Assets", name), png);
        writeFileSync(path.join(external, name), png);
      }
    }
    const priConfig = path.join(work, "priconfig.xml");
    checked(makepri, ["createconfig", "/cf", priConfig, "/dq", "en-US", "/o"], "makepri createconfig");
    checked(makepri, ["new", "/pr", stage, "/cf", priConfig, "/of", path.join(stage, "resources.pri"), "/o"], "makepri new");
    if (!existsSync(path.join(stage, "resources.pri"))) fail("resources.pri was not produced; the taskbar icon would be blank");
    rmSync(msix, { force: true });
    checked(makeappx, ["pack", "/o", "/d", stage, "/nv", "/p", msix], "makeappx pack");
    if (!existsSync(msix) || statSync(msix).size === 0) fail(`makeappx produced no package: ${msix}`);
    const roundTrip = path.join(work, "roundtrip");
    checked(makeappx, ["unpack", "/p", msix, "/d", roundTrip, "/o", "/nv"], "makeappx unpack round-trip");
    const missing = expectedPackageFiles(plan).filter((rel) => !existsSync(path.join(roundTrip, ...rel.split("/"))));
    if (missing.length) fail(`round-trip unpack of ${plan.package} is missing: ${missing.join(", ")}`);
    const externalMissing = [...plan.files.map((f) => f.to), ...scaledNames(plan)].filter((n) => !existsSync(path.join(external, n)));
    if (externalMissing.length) fail(`external Assets lacks files the PRI indexes: ${externalMissing.join(", ")}`);
    const dump = path.join(work, "pri-dump.xml");
    checked(makepri, ["dump", "/if", path.join(roundTrip, "resources.pri"), "/of", dump, "/o"], "makepri dump");
    const dumpXml = readFileSync(dump, "utf8");
    const unindexed = expectedPriUris(plan, stamped.name).filter((uri) => !dumpXml.includes(uri));
    if (unindexed.length) fail(`packaged resources.pri does not index: ${unindexed.join(", ")}`);
    // The one provisioned signer; it verifies after signing and exits non-zero on failure. No receipt file: nothing is written but the package.
    (deps.sign ?? ((file) => checked(process.execPath, [SIGN_WINDOWS, file], "sign-windows")))(msix);
    log(`signed ${plan.package}`);
  } catch (error) {
    rmSync(msix, { force: true }); // never leave an unchecked or unsigned package that a later run would take as present
    throw error;
  } finally { rmSync(work, { recursive: true, force: true }); }
  const files = [`${plan.out}/${plan.package}`, ...[...plan.files.map((f) => f.to), ...scaledNames(plan)].map((n) => `${plan.out}/external-assets/Assets/${n}`)];
  return { skipped: null, steps, out: plan.out, package: plan.package, files };
}

export function parseArgs(argv) {
  const opts = { config: null, json: false, dryRun: false };
  for (let i = 0; i < argv.length; i += 1) {
    if (argv[i] === "--config") { opts.config = argv[i + 1]; i += 1; if (!opts.config || opts.config.startsWith("--")) throw new Error("--config needs a value"); }
    else if (argv[i] === "--json") opts.json = true;
    else if (argv[i] === "--dry-run") opts.dryRun = true;
    else throw new Error(`unknown windows-identity argument: ${argv[i]}`);
  }
  if (!opts.config) throw new Error("--config <file> is required");
  return opts;
}

export async function main(argv = process.argv.slice(2), { env = process.env, platform = process.platform, spawnSyncFn = spawnSync, loadConfig = loadAppConfig, requireDoor, deps = {}, out = (s) => console.log(s) } = {}) {
  const opts = parseArgs(argv);
  const appRoot = path.dirname(path.resolve(opts.config));
  // The door first (a .mjs config is code), then the platform: a non-Windows host builds nothing and says so.
  const refusal = webBuildDoorRefusal({ appRoot, env, spawnSyncFn, ...(requireDoor ? { requireDoor } : {}) });
  if (refusal) fail(refusal.replace("right-release web-build", "windows-identity"));
  const result = (r) => { if (opts.json) out(JSON.stringify(r)); return 0; };
  if (platform !== "win32") return result({ skipped: "not-windows", files: [] });
  const { config } = await loadConfig(opts.config);
  const { problems, plan } = planWindowsIdentity(config);
  if (!plan) fail(problems.join("; "));
  if (opts.dryRun) return result({ skipped: "dry-run", steps: windowsIdentitySteps(plan), out: plan.out, package: plan.package, files: [] });
  return result(buildWindowsIdentity({ plan, appRoot }, { platform, env, ...deps }));
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? "").href) {
  main().then((code) => { process.exitCode = code; }, (error) => {
    console.error(`windows-identity: ${error.code ? `[${error.code}] ` : ""}${error.message}`);
    process.exitCode = 1;
  });
}
