// doctor == build. Three parts:
//  (i)   the inventory: every place the build chain refuses (fail(, reject(, throw new ...Error, process.exit(1)) is classified in
//        refusals.json, and a doctor-covered site names the row that covers it; an unlisted site fails.
//  (ii)  the ratchet: the build-only entries are counted, the count equals maxBuildOnly, and it only goes down, to 0.
//  (iii) the behaviour: one mutation per admission row. The doctor exits 1 naming the row and the build exits 1 with the same text
//        before any .right-release file, state directory or pnpm run exists. Rows that cannot run on this host are compared at
//        unit level, against the text the build's own module raises.
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import { existsSync, mkdirSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import path, { delimiter } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { spawnSync } from "./background-process.mjs";
import { ADMISSION_ROWS, runAdmission } from "./admission.mjs";
import { sccacheRefusal } from "./admission-host.mjs";
import { resolveMacosDeveloperIdIdentity } from "./macos-signing-identity.mjs";
import { cargoFfmpegCrate } from "./media-sidecar-catalog.mjs";
import { notarytoolAuthArgs } from "./notary-auth.mjs";
import { isManagedHost } from "./managed-host-secrets.mjs";
import { execWithUpdaterCredential } from "./protected-credentials.mjs";
import { evaluatePrebuildGates } from "./prebuild-gates.mjs";
import { assertNativeIosPrepareHasManifest } from "./release-invocation.mjs";
import { scanRefusalSiteDetails, scanRefusalSites } from "./test-support/refusal-sites.mjs";
import { build, editConfig, fakeTools, fixture, git, managedEnv, release, versions } from "./test-support/fixture.mjs";
import { tempRoot } from "./test-support/temp-roots.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));
const read = (file) => readFileSync(path.join(here, file), "utf8");

// ------------------------------------------------------------------------------------------------------------- (i) the inventory
// The build chain: the doctor and the worker, the controller, and every module whose refusal ends a build or an upload.
export const SCANNED_FILES = Object.freeze([
  "release.mjs", "build-release.mjs", "release-invocation.mjs", "prebuild-gates.mjs", "source-gate.mjs", "nsis-upgrade-contract.mjs",
  "release-checks.mjs", "publish-update.mjs", "sign-updater.mjs", "sign-windows.mjs", "sign-macos.mjs", "macos-signing-identity.mjs",
  "notary-auth.mjs", "protected-credentials.mjs", "managed-host-secrets.mjs", "rightkit-registry-resolver.mjs", "release-state.mjs",
]);
const CLASSES = new Set(["doctor-covered", "doctor-informational", "build-only-files", "build-only-host", "runtime-only"]);
const refusals = JSON.parse(read("refusals.json"));
const entryByKey = new Map(refusals.entries.map((entry) => [entry.key, entry]));
const scanned = SCANNED_FILES.flatMap((file) => scanRefusalSiteDetails(read(file), file).map((site) => ({ ...site, file })));

// The rows a doctor-covered site may name: an admission row, the literal name of a check( / reject( / rejectBlocked( in release.mjs,
// or a name release-config.mjs gives a field refusal that release.mjs lists with reject(refusal.name, ...).
function knownDoctorRows() {
  const names = new Set(ADMISSION_ROWS.map((row) => row.id));
  for (const match of read("release.mjs").matchAll(/\b(?:check|reject|rejectBlocked)\(\s*"([^"]+)"/g)) names.add(match[1]);
  for (const match of read("release-config.mjs").matchAll(/\bname:\s*"([^"]+)"/g)) names.add(match[1]);
  return names;
}

test("every refusal site of the build chain is classified in refusals.json (an unlisted site fails)", () => {
  const unlisted = scanned.filter((site) => !entryByKey.has(site.key));
  assert.deepEqual(unlisted.map((site) => `${site.file}:${site.line} ${site.kind} ${site.key}  ${site.message.slice(0, 80)}`), [],
    "classify each site in packages/release/refusals.json: doctor-covered (name the row), doctor-informational, build-only-files, build-only-host or runtime-only (write the reason)");
});

test("refusals.json lists no site that no longer exists", () => {
  const live = new Set(scanned.map((site) => site.key));
  assert.deepEqual(refusals.entries.filter((entry) => !live.has(entry.key)).map((entry) => entry.key), []);
});

test("a refusal added to a scanned file is seen by the scanner and has no entry", () => {
  const source = `${read("release-invocation.mjs")}\nfunction aBrandNewGate(x) { if (!x) fail("a brand new refusal nobody classified"); }\n`;
  const added = scanRefusalSites(source, "release-invocation.mjs").filter((key) => !entryByKey.has(key));
  assert.equal(added.length, 1);
  assert.match(added[0], /^release-invocation\.mjs::aBrandNewGate::[0-9a-f]{8}$/);
});

test("the scanner finds fail(, reject(name, detail), throw new ...Error and process.exit(1), and skips what is not a refusal", () => {
  const source = [
    'const a = "fail(\\"in a string\\")"; // fail("in a comment")',
    "const re = /fail\\(\"x\"\\)[}`]/g;",
    "function one() { fail(`dirty ${files.join(',')}`); }",
    'function two() { reject("row-name", `detail ${x}`); }',
    "const three = (v) => { throw new TypeError(`bad ${v}`); };",
    "async function four() { throw new RangeError(\"range\"); }",
    "function five() { process.exit(1); process.exit(2); process.exit(code ?? 1); }",
    "function six() { return new Promise((resolve, reject) => { reject(error); }); }",
    "function seven() { throw error; }",
    "function eight() { throw new ReleaseRefusal(\"named\", \"row\"); }",
    "function fail(message) { console.error(message); }",
    "await check(\"row-x\", [], () => { throw new Error(\"in a check\"); });",
  ].join("\n");
  const sites = scanRefusalSiteDetails(source, "x.mjs");
  assert.deepEqual(sites.map((site) => [site.kind, site.fn, site.check, site.name]), [
    ["fail", "one", null, null],
    ["reject", "two", null, "row-name"],
    ["throw", "three", null, null],
    ["throw", "four", null, null],
    ["exit", "five", null, null],
    ["throw", "eight", null, null],
    ["throw", "<module>", "row-x", null],
  ]);
  // the key follows the message, not the line; a moved site keeps its key, a changed message does not
  const moved = scanRefusalSites(`\n\n\n${source}`, "x.mjs");
  assert.deepEqual(moved, sites.map((site) => site.key));
  assert.notDeepEqual(scanRefusalSites(source.replace("range", "other range"), "x.mjs"), moved);
  // two sites with the same function and message are told apart
  assert.deepEqual(scanRefusalSites('function f(a) { if (a) fail("same"); fail("same"); }', "y.mjs").map((key) => key.replace(/[0-9a-f]{8}/, "#")), ["y.mjs::f::#", "y.mjs::f::##2"]);
});

test("every entry is well formed: a class, the row that covers it, a written reason where one is owed", () => {
  const rows = knownDoctorRows();
  for (const entry of refusals.entries) {
    assert.ok(CLASSES.has(entry.class), `${entry.key}: class ${entry.class}`);
    if (entry.class === "doctor-covered") assert.ok(rows.has(entry.doctorRow), `${entry.key}: doctorRow ${entry.doctorRow} is neither an admission row nor a check( / reject( name in release.mjs`);
    if (entry.class === "doctor-informational") assert.ok(entry.doctorRow && entry.reason, `${entry.key}: names how the doctor shows it and why it does not fail`);
    if (entry.class === "runtime-only") assert.ok(typeof entry.reason === "string" && entry.reason.length >= 20, `${entry.key}: a runtime-only site gives its reason`);
    if (entry.class.startsWith("build-only")) assert.ok(entry.issue && entry.reason, `${entry.key}: a build-only site names its issue and why the doctor cannot see it yet`);
  }
});

test("no refusal inside a check() callback leaves the process: the sites that call fail( or exit(1) there are plumbing", () => {
  const leaving = scanned.filter((site) => site.file === "release.mjs" && site.check && (site.kind === "fail" || site.kind === "exit"));
  assert.deepEqual(leaving.map((site) => site.key), []);
});

// ------------------------------------------------------------------------------------------------------------------ (ii) the ratchet
// The build-only count at the start of this work (C0). Lower both numbers when a site gets its row; never raise either.
const BUILD_ONLY_AT_C0 = 53;

test("the build-only entries equal maxBuildOnly, and maxBuildOnly only goes down", () => {
  const buildOnly = refusals.entries.filter((entry) => entry.class.startsWith("build-only"));
  assert.equal(buildOnly.length, refusals.maxBuildOnly, "update maxBuildOnly in refusals.json to the number of build-only entries (it may only decrease)");
  assert.ok(refusals.maxBuildOnly <= BUILD_ONLY_AT_C0, `maxBuildOnly ${refusals.maxBuildOnly} is above the ${BUILD_ONLY_AT_C0} it started at`);
  assert.equal(buildOnly.filter((entry) => entry.class === "build-only-host").length, 0, "every host-deterministic refusal has a host row");
});

// -------------------------------------------------------------------------------------------------------------- (iii) the behaviour
const WIN_NSIS = "windows-raw-exe-authenticode-before-nsis-v1";
const WIN_PORTABLE = "windows-raw-exe-authenticode-before-portable-v1";

/** One mutation of a fixture app. `kind`: both = the doctor and the build; worker = the doctor and the worker; doctor = the doctor only; unit = a function. */
export const PARITY_CASES = [
  // ---- files tier, the doctor and the build
  { id: "config-identity", rows: ["config-identity"], platform: "win", edit: (v) => { v.app = "bad app"; }, text: "app and version must be safe release-id components" },
  { id: "package-command", rows: ["package-command"], platform: "mac", options: { macSigning: true }, edit: (v) => { delete v.targets.mac.package; }, text: "fixture has no mac package command" },
  { id: "build-inputs (the rightkit#946 repro)", rows: ["build-inputs"], platform: "mac", options: { macSigning: true, buildInputs: null }, text: "release config must declare non-empty buildInputs.include paths" },
  { id: "dirty-inputs", rows: ["dirty-inputs"], platform: "mac", options: { macSigning: true }, dirty: (dir) => writeFileSync(path.join(dir, "package.json"), `${readFileSync(path.join(dir, "package.json"), "utf8")}\n`), text: "dirty files can change the packaged app; resolve them before release:\n- package.json" },
  { id: "input-key-collision", rows: ["input-key-collision"], platform: "mac", options: { macSigning: true },
    edit: (v, config) => {
      const dir = path.dirname(config);
      mkdirSync(path.join(dir, path.basename(dir)));
      writeFileSync(path.join(dir, path.basename(dir), "package.json"), "{}\n");
      v.targets.mac.preflight = { files: [`${path.basename(dir)}/package.json`] };
    },
    text: (config) => `release inputs share the hash key ${path.join(path.basename(path.dirname(config)), "package.json")}; rename one so every input is sealed` },
  { id: "nsis-upgrade-contract", rows: ["nsis-upgrade-contract"], platform: "win", edit: (v, config) => writeFileSync(path.join(path.dirname(config), "src-tauri", "tauri.conf.json"), "{}\n"), text: "Windows releases require a custom NSIS template with the RightKit automatic in-place upgrade contract" },
  { id: "win-updater-artifacts (the desktop-app-template's win target)", rows: ["win-updater-artifacts"], alsoRows: ["seal-routes"], platform: "win", edit: (v) => { v.targets.win.updater.artifacts = []; }, text: "fixture win must declare updater.artifacts for post-code-signing minisign" },
  { id: "seal-routes: no updater public key", rows: ["seal-routes"], platform: "win", edit: (v) => { delete v.distribution.updaterPublicKey; }, text: "updater public key is required to verify updater artifacts before sealing" },
  { id: "seal-routes: nothing to seal", rows: ["seal-routes"], alsoRows: ["win-updater-artifacts"], platform: "win", edit: (v) => { v.targets.win.updater.artifacts = []; v.targets.win.artifacts = []; }, text: "nothing to seal: declare installer.artifacts, updater.artifacts or artifacts for the win target" },
  { id: "seal-routes: an updater artifact with no route key (the TypeError after compile)", rows: ["seal-routes"], platform: "win", edit: (v) => { delete v.targets.win.updater.artifacts[0].key; }, text: "updater artifact fixture.exe must declare the R2 route key (key); only github-releases names the route from the file" },
  { id: "seal-routes: an updater key that is not the current R2 object (publish-update.mjs assertCurrentKey, both tiers)", rows: ["seal-routes"], platform: "win", edit: (v) => { v.targets.win.updater.artifacts[0].key = "fixture/fixture.exe"; }, text: "updater key must target the replaceable current R2 object, got: fixture/fixture.exe" },
  { id: "seal-routes: the patch tier's installer key (only a config with installer.artifacts has a patch tier)", rows: ["seal-routes"], platform: "win", edit: (v) => { v.targets.win.installer = { artifacts: [{ file: "fixture-setup.exe", key: "fixture/setup.exe" }] }; }, text: "installer key must target the replaceable current R2 object, got: fixture/setup.exe" },
  { id: "seal-routes: an installer with no key (publish-update.mjs --tier patch: got: undefined)", rows: ["seal-routes"], platform: "win", edit: (v) => { v.targets.win.installer = { artifacts: [{ file: "fixture-setup.exe" }] }; }, text: "installer key must target the replaceable current R2 object, got: undefined" },
  { id: "distribution-provider", rows: ["distribution-provider"], platform: "win", edit: (v) => { v.distribution.provider = "r2-postrights"; }, text: 'release config distribution.provider must be r2-rightapps or github-releases (got "r2-postrights")' },
  { id: "distribution-provider: no distribution at all (publish-update.mjs refuses it)", rows: ["distribution-provider"], alsoRows: ["seal-routes"], platform: "win", edit: (v) => { delete v.distribution; }, text: "release config distribution.provider must be r2-rightapps or github-releases" },
  { id: "media-sidecar-plan: a catalog the config names with no row for a tool", rows: ["media-sidecar-plan"], platform: "win",
    edit: (v, config) => {
      const dir = path.dirname(config);
      mkdirSync(path.join(dir, "vendor", "ffmpeg", "catalog"), { recursive: true });
      writeFileSync(path.join(dir, "vendor", "ffmpeg", "catalog", "artifacts.json"), JSON.stringify({ schemaVersion: 2, artifacts: [] }));
      v.targets.win.mediaSidecar = { arch: "x86_64", catalog: "vendor/ffmpeg/catalog/artifacts.json", notices: { dir: "src-tauri/resources/ffmpeg", sourceOfferContact: "legal@example.test" } };
    },
    text: "fixture win mediaSidecar: [MEDIA_SIDECAR_NO_ROW] the rightkit-ffmpeg catalog has no ffmpeg decode windows/x86_64 row" },
  { id: "frontend-config-readable", rows: ["frontend-config-readable"], platform: "mac", options: { macSigning: true }, edit: (v, config) => { mkdirSync(path.join(path.dirname(config), "src-tauri"), { recursive: true }); writeFileSync(path.join(path.dirname(config), "src-tauri", "tauri.conf.json"), "{ not json"); }, text: "src-tauri/tauri.conf.json is unreadable, so the frontend bundle cannot be located for the mock scan:" },
  { id: "declared-checks-exist", rows: ["declared-checks-exist"], platform: "mac", options: { macSigning: true }, edit: (v) => { v.checks = ["typecheck"]; }, text: 'config.checks names "typecheck", which is not a script in package.json' },
  { id: "sidecar-app-entry", rows: ["sidecar-app-entry"], platform: "mac", options: { macSigning: true }, edit: (v) => { v.targets.mac.mediaSidecar = {}; }, text: "mediaSidecar on macOS needs an .app entry in sign.prePackageFiles" },
  { id: "pre-seal-command", rows: ["pre-seal-command"], platform: "mac", options: { macSigning: true }, edit: (v) => { v.targets.mac.preSeal = {}; }, text: "release preSeal check must declare a command" },
  // ---- network tier
  { id: "rightkit-registry: a stale pin", rows: ["rightkit-registry"], platform: "mac", options: { macSigning: true }, env: { RK_TEST_LATEST: "@rightkit/license@@99.0.0" }, text: `fixture RightKit pins are behind the latest published version (build admission refused):\n- @rightkit/license pinned ${versions.npm["@rightkit/license"]} -> latest 99.0.0` },
  // ---- host tier, the doctor and the build
  { id: "host-secret-env: TAURI_SIGNING_PRIVATE_KEY in the shell", rows: ["host-secret-env"], platform: "win", env: { TAURI_SIGNING_PRIVATE_KEY: "value-7f3a1" }, text: () => isManagedHost(process.env) ? "TAURI_SIGNING_PRIVATE_KEY is refused on a managed RightKit host; load the secret from the provisioned OS credential store" : "updater signing key/password must come from the OS credential provider" },
  { id: "host-tools: the pnpm on this host is not the pin", rows: ["host-tools"], platform: "mac", options: { macSigning: true }, env: () => ({ PATH: `${fakeTools({ pnpm: "10.0.0" })}${delimiter}${process.env.PATH}` }), text: `pnpm 10.0.0 is installed but package.json packageManager pins ${versions.packageManager}; install the pinned version (corepack prepare ${versions.packageManager} --activate)` },
  { id: "host-preflight-env", rows: ["host-preflight-env"], platform: "mac", options: { macSigning: true }, edit: (v) => { v.targets.mac.preflight = { env: ["RIGHT_RELEASE_TEST_ABSENT_VARIABLE"] }; }, text: "missing required environment variable: RIGHT_RELEASE_TEST_ABSENT_VARIABLE" },
  // ---- doctor only
  { id: "declared-checks-run: a typecheck that fails", rows: ["declared-checks-run"], kind: "doctor", platform: "mac", options: { macSigning: true, configExtra: { checks: ["typecheck"] }, packageScripts: { typecheck: "tsc --noEmit" } },
    dirty: (dir) => { mkdirSync(path.join(dir, "node_modules")); writeFileSync(path.join(dir, "node_modules", ".keep"), ""); }, env: () => ({ PATH: `${fakeTools({ pnpmRunExit: 1 })}${delimiter}${process.env.PATH}` }), text: 'check "typecheck" failed (pnpm run typecheck exited 1)' },
  { id: "artifact-storage", rows: ["artifact-storage"], kind: "doctor", platform: "mac", options: { macSigning: true, packageScripts: { "copy:installer": "cp dist/App.dmg ." } }, text: "artifact-storage found 1 repository storage finding(s); move outputs to RIGHT_RELEASE_ARTIFACT_DIR" },
  // ---- the config rules the doctor and the worker share (the worker refuses them at its checkpoint; the controller still takes them there)
  { id: "target-signed", rows: ["target-signed"], kind: "worker", platform: "win", edit: (v) => { v.targets.win.signed = false; }, text: "fixture win must declare signed: true; unsigned release targets are forbidden" },
  { id: "target: no release target for the platform", rows: ["target"], kind: "worker", platform: "win", edit: (v) => { delete v.targets.win; }, text: "fixture has no win release target" },
  { id: "win-signing-contract", rows: ["win-signing-contract", "win-contract-rules"], kind: "worker", platform: "win", edit: (v) => { delete v.targets.win.signingContract; }, text: `fixture win must declare signingContract: ${WIN_NSIS}, ${WIN_PORTABLE} or windows-msix-signed-v1` },
  { id: "win-prepackage", rows: ["win-prepackage"], kind: "worker", platform: "win", edit: (v) => { delete v.targets.win.prePackage; }, text: "fixture win must declare prePackage for the raw EXE build" },
  { id: "win-sign-prepackage-files", rows: ["win-sign-prepackage-files"], kind: "worker", platform: "win", edit: (v) => { v.targets.win.sign.prePackageFiles = []; }, text: "fixture win must declare at least one sign.prePackageFiles raw EXE" },
  { id: "win-sign-entries", rows: ["win-sign-entries"], kind: "worker", platform: "win", edit: (v) => { v.targets.win.sign.prePackageFiles = [{ file: "raw.exe" }]; }, text: "fixture win sign.prePackageFiles entries must be path strings; object entries and sign.nestedCode are macOS-only" },
  { id: "win-nsis-raw-exe", rows: ["win-nsis-raw-exe"], kind: "worker", platform: "win", edit: (v) => { v.targets.win.sign.prePackageFiles = ["raw.exe", "other.exe"]; }, text: "fixture win NSIS must declare exactly one sign.prePackageFiles raw EXE" },
  { id: "win-nsis-sign-files", rows: ["win-nsis-sign-files"], kind: "worker", platform: "win", edit: (v) => { v.targets.win.sign.files = []; }, text: "fixture win NSIS must declare sign.files for installer signing" },
  { id: "win-distinct-files", rows: ["win-distinct-files"], kind: "worker", platform: "win", edit: (v) => { v.targets.win.sign.files = ["raw.exe"]; }, text: "fixture win raw EXE and installer signing files must be distinct" },
  { id: "win-portable-sign-files", rows: ["win-portable-sign-files"], kind: "worker", platform: "win", edit: (v) => { v.targets.win.signingContract = WIN_PORTABLE; }, text: "fixture win portable release must sign every executable through sign.prePackageFiles and omit sign.files" },
  { id: "mac-signing-contract", rows: ["mac-signing-contract"], kind: "worker", platform: "mac", options: { macSigning: true }, edit: (v) => { delete v.targets.mac.signingContract; }, text: "fixture mac must declare signingContract: macos-developer-id-notarized-portable-v1" },
  { id: "mac-prepackage", rows: ["mac-prepackage"], kind: "worker", platform: "mac", options: { macSigning: true }, edit: (v) => { delete v.targets.mac.prePackage; }, text: "fixture mac must declare prePackage to materialize Developer ID signing files" },
  { id: "mac-sign-prepackage-files", rows: ["mac-sign-prepackage-files"], kind: "worker", platform: "mac", options: { macSigning: true }, edit: (v) => { v.targets.mac.sign.prePackageFiles = []; }, text: "fixture mac must declare at least one sign.prePackageFiles" },
  { id: "mac-notarize-file", rows: ["mac-notarize-file"], kind: "worker", platform: "mac", options: { macSigning: true }, edit: (v) => { delete v.targets.mac.notarize.file; }, text: "fixture mac must declare notarize.file" },
  { id: "mac-notarize-app", rows: ["mac-notarize-app"], kind: "worker", platform: "mac", options: { macSigning: true }, edit: (v) => { v.targets.mac.notarize.appBeforePackage = true; }, text: "fixture mac notarize.appBeforePackage requires notarize.app" },
  { id: "mac-notarize-staple", rows: ["mac-notarize-staple"], kind: "worker", platform: "mac", options: { macSigning: true }, edit: (v) => { v.targets.mac.notarize.staple = false; v.targets.mac.notarize.file = "fixture.dmg"; }, text: "fixture mac notarize.staple: false applies only to an archive of bare executables" },
  { id: "media-sidecar on a platform that has none", rows: ["media-sidecar"], kind: "worker", platform: "linux", edit: (v) => { v.targets.linux.mediaSidecar = {}; }, text: "fixture linux does not support mediaSidecar (mac and win only)" },
  { id: "external-bin on a platform that has none", rows: ["external-bin"], kind: "worker", platform: "linux", edit: (v) => { v.targets.linux.externalBin = []; }, text: "fixture linux does not support externalBin (mac and win only)" },
  { id: "ios-target", rows: ["ios-target"], kind: "worker", platform: "ios", text: "fixture ios must declare targets.ios.iosPackage; there is no hand-written iOS package script" },
  { id: "android-target", rows: ["android-target"], kind: "worker", platform: "android", text: "fixture android " },
  { id: "updater-key-file", rows: ["updater-key-file"], kind: "worker", platform: "win", edit: (v) => { v.targets.win.updater.keyFile = "%APPDATA%/RightKit/updater.key"; }, text: "targets.win.updater.keyFile is not supported: updater signing keys come from the OS credential provider" },
  { id: "package-manager", rows: ["package-manager"], kind: "worker", platform: "win", options: { packageJson: { name: "fixture", scripts: {}, packageManager: "pnpm@9.0.0", dependencies: { "@rightkit/license": versions.npm["@rightkit/license"] }, devDependencies: { "@rightkit/release": versions.npm["@rightkit/release"] } } }, text: `fixture packageManager must be ${versions.packageManager}, got pnpm@9.0.0` },
  { id: "package-workspace-scripts", rows: ["package-workspace-scripts"], kind: "worker", platform: "win", options: { packageJson: { name: "fixture", scripts: { release: "node ../tools/right-release/cli.mjs" }, packageManager: versions.packageManager, dependencies: { "@rightkit/license": versions.npm["@rightkit/license"] }, devDependencies: { "@rightkit/release": versions.npm["@rightkit/release"] } } }, text: "fixture package.json must call the right-release bin, not parent-workspace release source" },
  { id: "rightkit-pin-specs", rows: ["rightkit-pin-specs"], kind: "worker", platform: "win", options: { packageJson: { name: "fixture", scripts: {}, packageManager: versions.packageManager, dependencies: { "@rightkit/license": "git+https://github.com/bogusyogi/rightkit.git" }, devDependencies: { "@rightkit/release": versions.npm["@rightkit/release"] } } }, text: "fixture @rightkit/license must come from the published npm package, not git+https://github.com/bogusyogi/rightkit.git" },
  { id: "rightkit-release-supported", rows: ["rightkit-release-supported"], kind: "worker", platform: "win", options: { packageJson: { name: "fixture", scripts: {}, packageManager: versions.packageManager, dependencies: { "@rightkit/license": versions.npm["@rightkit/license"] }, devDependencies: {} } }, text: "fixture must depend on a supported @rightkit/release version in devDependencies" },
  { id: "hosted-workflows", rows: ["hosted-workflows"], kind: "worker", platform: "win", workflow: true, text: "fixture hosted workflow files are forbidden: ci.yml" },
];

/** Rows whose probe needs a Windows host, a Mac or a live Keychain, compared against the text the build's own module raises. */
export const UNIT_CASES = [
  { id: "host-updater-credential", rows: ["host-updater-credential"],
    async run() {
      const winHost = { hostPlatform: "win32", env: { PATH: "C:\\bin", PATHEXT: ".EXE" }, fs: { existsSync: () => false, readFileSync: () => "", readdirSync: () => [] }, spawn: () => ({ status: 0 }) };
      const result = await runAdmission({ root: "/app", config: { app: "fixture" }, platform: "win", target: { updater: { artifacts: [{ file: "a.exe" }] } }, tiers: ["host"], rows: ADMISSION_ROWS.filter((row) => row.id === "host-updater-credential"), ...winHost });
      const raised = (() => { try { execWithUpdaterCredential(path.resolve("/x"), [], { env: {}, run: () => ({ error: { code: "ENOENT" } }) }); } catch (error) { return error.message; } })();
      return { row: result.findings[0]?.row, found: result.findings.map((finding) => finding.detail), built: [raised] };
    } },
  { id: "host-signing-win", rows: ["host-signing-win"],
    async run() {
      const winHost = { hostPlatform: "win32", env: {}, fs: { existsSync: () => false, readFileSync: () => "", readdirSync: () => [] }, spawn: () => ({ status: 0, stdout: "" }) };
      const result = await runAdmission({ root: "/app", config: { app: "fixture" }, platform: "win", target: { signingContract: WIN_NSIS }, tiers: ["host"], rows: ADMISSION_ROWS.filter((row) => row.id === "host-signing-win"), ...winHost });
      // sign-windows.mjs is a script, so its texts are read from its source: each finding is a message it raises.
      const source = read("sign-windows.mjs");
      const raised = result.findings.map((finding) => finding.detail).filter((detail) => source.includes(JSON.stringify(detail)));
      return { row: result.findings[0]?.row, found: result.findings.map((finding) => finding.detail), built: raised };
    } },
  { id: "host-signing-mac", rows: ["host-signing-mac"],
    async run() {
      const mac = { hostPlatform: "darwin", env: { PATH: "/bin" }, fs: { existsSync: () => false, readFileSync: () => "", readdirSync: () => [] }, spawn: (file, args) => (args[0] === "find-identity" ? { status: 0, stdout: "     0 valid identities found\n" } : { status: 0, stdout: "" }) };
      const result = await runAdmission({ root: "/app", config: { app: "fixture" }, platform: "mac", target: { notarize: { file: "a.dmg", profile: "" } }, tiers: ["host"], rows: ADMISSION_ROWS.filter((row) => row.id === "host-signing-mac"), ...mac });
      const built = [];
      try { resolveMacosDeveloperIdIdentity({ env: {}, run: mac.spawn }); } catch (error) { built.push(error.message); }
      try { notarytoolAuthArgs({ profile: "", env: {} }); } catch (error) { built.push(error.message); }
      return { row: result.findings[0]?.row, found: result.findings.map((finding) => finding.detail), built };
    } },
  { id: "host-cache-mode", rows: ["host-cache-mode"],
    async run() {
      const host = { hostPlatform: "linux", env: { PATH: "/bin", RIGHT_RELEASE_CACHE_MODE: "shared" }, fs: { existsSync: (file) => file === "/bin/sccache", readFileSync: () => "", readdirSync: () => [] }, spawn: () => ({ status: 0, stdout: "sccache 0.14.0\n" }) };
      const result = await runAdmission({ root: "/app", config: { app: "fixture" }, platform: "win", target: {}, tiers: ["host"], rows: ADMISSION_ROWS.filter((row) => row.id === "host-cache-mode"), ...host });
      // the doctor's own prelude (release.mjs) and the build's late check (build-release.mjs) take their text from the same helper
      assert.match(read("release.mjs"), /fail\(sccacheRefusal\(version\?\.\[0\]\)\)/);
      assert.match(read("build-release.mjs"), /fail\(sccacheRefusal\(/);
      return { row: result.findings[0]?.row, found: result.findings.map((finding) => finding.detail), built: [sccacheRefusal("0.14.0")] };
    } },
  { id: "media-sidecar-plan: no native Cargo manifest", rows: ["media-sidecar-plan"],
    async run() {
      const sidecar = { arch: "x86_64", notices: { dir: "n", sourceOfferContact: "legal@example.test" } };
      const result = await runAdmission({ root: "/app", config: { app: "fixture" }, platform: "win", target: { mediaSidecar: sidecar }, tiers: ["files"], rows: ADMISSION_ROWS.filter((row) => row.id === "media-sidecar-plan"), resolveInputs: () => ({ ok: true, value: { nativeLayout: null } }) });
      // release.mjs prepareMediaSidecar says it with its own template; the row's finding is that template filled in
      const said = /fail\(`\$\{config\.app \?\? "app"\} \$\{opts\.platform\} (mediaSidecar needs a native Cargo manifest to read the sidecar catalog, and this app has none)`\)/.exec(read("release.mjs"))?.[1];
      return { row: result.findings[0]?.row, found: result.findings.map((finding) => finding.detail), built: said ? [`fixture win ${said}`] : [] };
    } },
  { id: "media-sidecar-catalog", rows: ["media-sidecar-catalog"],
    async run() {
      const sidecar = { arch: "x86_64", notices: { dir: "n", sourceOfferContact: "legal@example.test" } };
      const cargoFailed = () => ({ status: 101, stdout: "", stderr: "error: no lock\n" });
      const host = { hostPlatform: "linux", env: { PATH: "/bin" }, fs: { existsSync: () => false, readFileSync: () => "", readdirSync: () => [] }, spawn: cargoFailed };
      const result = await runAdmission({ root: "/app", config: { app: "fixture" }, platform: "win", target: { mediaSidecar: sidecar }, tiers: ["host"], rows: ADMISSION_ROWS.filter((row) => row.id === "media-sidecar-catalog"), resolveInputs: () => ({ ok: true, value: { nativeLayout: { manifestPath: path.resolve("/app/src-tauri/Cargo.toml") } } }), ...host });
      // the module the worker calls raises the refusal; release.mjs wraps it in the template the row repeats
      const raised = (() => { try { cargoFfmpegCrate(path.resolve("/app/src-tauri/Cargo.toml"), { run: cargoFailed }); } catch (error) { return `fixture win mediaSidecar: [${error.code ?? "ERROR"}] ${error.message}`; } })();
      assert.match(read("release.mjs"), /fail\(`\$\{config\.app \?\? "app"\} \$\{opts\.platform\} mediaSidecar: \[\$\{error\.code \?\? "ERROR"\}\] \$\{error\.message\}`\)/);
      return { row: result.findings[0]?.row, found: result.findings.map((finding) => finding.detail), built: [raised] };
    } },
];

function makeApp(testCase) {
  const platform = testCase.platform;
  const options = { platform, ...(testCase.options ?? {}) };
  if (options.packageScripts) {
    options.packageJson = { name: "fixture", scripts: options.packageScripts, packageManager: versions.packageManager, dependencies: { "@rightkit/license": versions.npm["@rightkit/license"], "@rightkit/logs": versions.npm["@rightkit/logs"] }, devDependencies: { "@rightkit/release": versions.npm["@rightkit/release"] } };
    delete options.packageScripts;
  }
  let config = fixture(options);
  if (testCase.edit) config = editConfig(config, testCase.edit);
  const dir = path.dirname(config);
  if (testCase.workflow) {
    mkdirSync(path.join(dir, ".github", "workflows"), { recursive: true });
    writeFileSync(path.join(dir, ".github", "workflows", "ci.yml"), "name: ci\non: push\njobs: {}\n");
  }
  git(dir, "add", ".");
  if (git(dir, "status", "--porcelain")) git(dir, "commit", "-m", "mutate");
  if (testCase.dirty) testCase.dirty(dir);
  return config;
}

const envOf = (testCase) => (typeof testCase.env === "function" ? testCase.env() : testCase.env ?? {});
const textOf = (testCase, config) => (typeof testCase.text === "function" ? testCase.text(config) : testCase.text);
const output = (result) => `${result.stdout}\n${result.stderr}`;
const stackFree = (result) => assert.doesNotMatch(result.stderr, /\n\s+at .*\.mjs:\d+/, "a refusal prints its message, never a stack");

function runDoctorOf(config, testCase, extra = []) {
  return spawnSync(process.execPath, [release, "--doctor", "--config", config, "--platform", testCase.platform, ...extra], { encoding: "utf8", env: managedEnv(config, { platform: testCase.platform, env: envOf(testCase) }) });
}

// The pnpm shim logs every call: the host probe asks its version, and nothing installs or runs.
function runBuildOf(config, testCase) {
  const log = path.join(tempRoot("doctor-parity-log-"), "calls.log");
  const root = path.dirname(config);
  const before = readdirSync(root).sort();
  const result = spawnSync(process.execPath, [build, "--config", config, "--platform", testCase.platform], {
    cwd: root, encoding: "utf8", env: managedEnv(config, { platform: testCase.platform, env: { ...envOf(testCase), RIGHT_RELEASE_TEST_TOOL_LOG: log } }),
  });
  const calls = existsSync(log) ? readFileSync(log, "utf8").split("\n").filter(Boolean) : [];
  return { result, root, before, calls };
}

for (const testCase of PARITY_CASES) {
  const kind = testCase.kind ?? "both";
  test(`parity [${kind}] ${testCase.id}`, () => {
    const config = makeApp(testCase);
    const text = textOf(testCase, config);
    const doctor = runDoctorOf(config, testCase);
    assert.equal(doctor.status, 1, `the doctor must refuse\n${output(doctor)}`);
    assert.ok(output(doctor).includes(text), `the doctor names it\n--- wanted\n${text}\n--- got\n${output(doctor)}`);
    stackFree(doctor);
    if (kind === "both") {
      const row = testCase.rows[0];
      assert.ok(doctor.stderr.includes(`right-release: ${row}: ${text}`), `the doctor names the row ${row}\n${doctor.stderr}`);
      const built = runBuildOf(config, testCase);
      assert.equal(built.result.status, 1, `the build must refuse the same fixture\n${output(built.result)}`);
      assert.ok(built.result.stderr.includes(`right-release build: ${row}: ${text}`), `the build says the same, with the row\n--- wanted\n${row}: ${text}\n--- got\n${built.result.stderr}`);
      stackFree(built.result);
      // the build found the case's own row (and what it cascades into) and nothing else: no host probe failed for want of a tool
      const found = [...new Set([...built.result.stderr.matchAll(/^right-release build: ([a-z][a-z-]*): /gm)].map((match) => match[1]))].sort();
      assert.deepEqual(found, [...new Set([...testCase.rows, ...(testCase.alsoRows ?? [])])].sort(), built.result.stderr);
      // before anything exists: no vault (locks, state, sealed, registry snapshot), nothing new in the app, no pnpm beyond its version
      assert.equal(existsSync(path.join(built.root, ".right-release")), false, "no .right-release (locks, state directory, snapshot)");
      assert.deepEqual(readdirSync(built.root).sort(), built.before, "the build wrote nothing into the app");
      assert.deepEqual(built.calls.filter((call) => call.startsWith("pnpm ") && call !== "pnpm --version"), [], "no pnpm install or run");
    } else if (kind === "worker") {
      const worker = spawnSync(process.execPath, [release, "--config", config, "--platform", testCase.platform, "--dry-run"], { encoding: "utf8", env: managedEnv(config, { worker: true, platform: testCase.platform, env: envOf(testCase) }) });
      assert.equal(worker.status, 1, `the worker must refuse\n${output(worker)}`);
      assert.ok(worker.stderr.includes(`right-release: ${text}`) || worker.stderr.includes(text), `the worker says the same\n--- wanted\n${text}\n--- got\n${worker.stderr}`);
    }
  });
}

for (const testCase of UNIT_CASES) {
  test(`parity [unit] ${testCase.id}`, async () => {
    const { row, found, built } = await testCase.run();
    assert.equal(row, testCase.rows[0], "the row names itself");
    assert.ok(found.length > 0, "the row finds the problem");
    for (const detail of found) assert.ok(built.some((raised) => detail === raised || detail.includes(raised)), `the finding "${detail}" is a text the build's own module raises`);
  });
}

// The worker (release.mjs prepareMediaSidecar) refuses the catalog the row refuses, in the row's words: the same fixture, run as the worker.
test("the worker refuses the media sidecar catalog the row refuses, with the row's text", () => {
  const testCase = PARITY_CASES.find((candidate) => candidate.id.startsWith("media-sidecar-plan:"));
  const config = makeApp(testCase);
  const worker = spawnSync(process.execPath, [release, "--config", config, "--platform", "win", "--dry-run"], { encoding: "utf8", env: managedEnv(config, { worker: true, platform: "win" }) });
  assert.equal(worker.status, 1, output(worker));
  assert.ok(worker.stderr.includes(`right-release: ${textOf(testCase, config)}`), `the worker says the row's text\n${worker.stderr}`);
});

// The refusal behind refusals.json's proof for release-invocation.mjs assertNativeIosPrepareHasManifest (the bare cargo commands rule).
test("a native iOS prepare that is not bare cargo commands is refused once a Rust manifest is declared", () => {
  const appRoot = path.resolve("/app");
  const declared = { nativeAssembly: { cargoManifest: "engine/Cargo.toml" } };
  assert.throws(() => assertNativeIosPrepareHasManifest({ prepare: [{ cmd: "node", args: ["build.mjs"] }] }, declared, appRoot), /iosPackage\.prepare must be only bare cargo commands when a Rust manifest is declared[\s\S]*prepare\[0\] \(node\) is not a bare cargo command/);
  assert.throws(() => assertNativeIosPrepareHasManifest({ prepare: [{ cmd: "cargo", args: ["build", "--manifest-path", "engine/Cargo.toml"] }] }, declared, appRoot), /has no --locked/);
  assert.doesNotThrow(() => assertNativeIosPrepareHasManifest({ prepare: [{ cmd: "cargo", args: ["build", "--locked", "--manifest-path", "engine/Cargo.toml"] }] }, declared, appRoot));
});

// The refusals behind refusals.json's --gates proofs for the source switch inventory (prebuild-gates.mjs source-switch-roots and -prepare).
test("the source switch inventory gates refuse an invalid nonShipped list and an iOS prepare that runs outside the scan roots", () => {
  const quiet = (run) => {
    const log = console.log;
    console.log = () => {};
    try { return run(); } finally { console.log = log; }
  };
  const nonShipped = tempRoot("doctor-parity-gates-");
  writeFileSync(path.join(nonShipped, "switches.json"), "{}\n");
  const listed = quiet(() => evaluatePrebuildGates({ root: nonShipped, config: { app: "x", releaseSwitches: { nonShipped: "not-an-array" } }, target: {}, platform: "linux", strict: false }));
  assert.match(listed.findings.find((finding) => finding.name === "source-switch-roots")?.detail ?? "", /^source switch inventory: 1 invalid releaseSwitches\.nonShipped entry:\n- inventory-invalid: releaseSwitches\.nonShipped: must be an array/);
  const ios = tempRoot("doctor-parity-gates-ios-");
  git(ios, "init", "--initial-branch", "main");
  writeFileSync(path.join(ios, "switches.json"), "{}\n");
  mkdirSync(path.join(ios, "App"));
  const iosSpec = { project: "App/App.xcodeproj", prepare: [{ cmd: "cargo", args: ["build", "--locked", "--manifest-path", "engine/Cargo.toml"], cwd: "tools" }] };
  const config = { app: "x", targets: { ios: { iosPackage: iosSpec } } };
  const outside = quiet(() => evaluatePrebuildGates({ root: ios, config, target: config.targets.ios, platform: "ios", nativeIos: true, iosSpec, strict: false }));
  assert.match(outside.findings.find((finding) => finding.name === "source-switch-prepare")?.detail ?? "", /^source switch inventory: targets\.ios\.iosPackage\.prepare runs in tools, outside the iOS scan roots \(App\); list that directory in targets\.ios\.ownershipScope/);
});

// ---------------------------------------------------------------------------------------------------- every row and entry has a proof
const PROOFS_OF_ENTRIES = new Set(PARITY_CASES.concat(UNIT_CASES).flatMap((testCase) => testCase.rows));

test("every admission row has a parity case", () => {
  assert.deepEqual(ADMISSION_ROWS.map((row) => row.id).filter((id) => !PROOFS_OF_ENTRIES.has(id)), []);
});

test("every doctor-covered and doctor-informational entry is referenced by a parity case, or names an existing test that proves it", () => {
  const unproven = [];
  // A build-only entry has no row yet, so it has no case; the ratchet above counts it down to 0.
  for (const entry of refusals.entries.filter((candidate) => candidate.class.startsWith("doctor-"))) {
    if (PROOFS_OF_ENTRIES.has(entry.doctorRow)) continue;
    const proof = entry.proof;
    const exists = proof && read(proof.file).includes(`test(${JSON.stringify(proof.test)}`);
    if (!exists) unproven.push(`${entry.key} (${entry.doctorRow ?? entry.class})`);
  }
  assert.deepEqual(unproven, [], "add a PARITY_CASE for the row, or name an existing test as { file, test } in the entry's proof");
});
