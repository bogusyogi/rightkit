// The admission rows (admission.mjs, admission-files.mjs, admission-host.mjs) as units: injected env, spawn, file system and registry.
// The doctor and build parity of the same rows is doctor-parity.test.mjs.
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import test from "node:test";
import { ADMISSION_ROWS, TIERS, describeRegistryError, formatAdmission, formatNotRun, runAdmission } from "./admission.mjs";
import { spawnSync } from "./background-process.mjs";
import { ManagedHostEnvSecretError } from "./managed-host-secrets.mjs";
import { execWithUpdaterCredential } from "./protected-credentials.mjs";
import { RightKitRegistryError } from "./rightkit-registry-resolver.mjs";
import { tempRoot } from "./test-support/temp-roots.mjs";

const rowOf = (id) => ADMISSION_ROWS.find((row) => row.id === id);

/** Run one row; `details` is the finding texts, `row` its entry in the result. */
async function unit(id, { root = tempRoot("admission-unit-"), config = { schema: 1, app: "fixture", version: "1.0.0" }, platform = "win", target = {}, ...rest } = {}) {
  const row = rowOf(id);
  assert.ok(row, `no admission row ${id}`);
  const result = await runAdmission({ root, repoRoot: root, config, platform, target, tiers: [row.tier], rows: [row], commit: "0".repeat(40), ...rest });
  return { result, row: result.rows[0], details: result.findings.map((finding) => finding.detail) };
}

// ---- the runner ----------------------------------------------------------------------------------------------------------------
test("the rows are data: an id, a tier and a run function, unique ids, three tiers", () => {
  assert.deepEqual([...TIERS], ["files", "host", "network"]);
  assert.equal(new Set(ADMISSION_ROWS.map((row) => row.id)).size, ADMISSION_ROWS.length);
  for (const row of ADMISSION_ROWS) {
    assert.ok(TIERS.includes(row.tier), row.id);
    assert.equal(typeof row.run, "function", row.id);
  }
  for (const id of ["config-identity", "package-command", "nsis-upgrade-contract", "win-updater-artifacts", "seal-routes", "distribution-provider", "input-key-collision", "frontend-config-readable", "declared-checks-exist", "media-sidecar-plan"]) assert.equal(rowOf(id)?.tier, "files", id);
  for (const id of ["host-tools", "host-secret-env", "host-updater-credential", "host-signing-win", "host-signing-mac", "host-preflight-env", "host-cache-mode", "media-sidecar-catalog", "declared-checks-run"]) assert.equal(rowOf(id)?.tier, "host", id);
});

test("runAdmission reports findings per row, never a pass for a row that could not run, and a throwing row is a finding", async () => {
  const rows = [
    { id: "passes", tier: "files", needsTarget: false, run: () => undefined },
    { id: "fails", tier: "files", needsTarget: false, run: () => ["first problem", "second problem"] },
    { id: "cannot-run", tier: "host", needsTarget: false, run: () => ({ notRun: "needs a Windows host" }) },
    { id: "not-applicable", tier: "host", needsTarget: false, run: () => ({ na: "no such target" }) },
    { id: "elsewhere", tier: "host", needsTarget: false, run: () => ({ skipped: "another step runs it" }) },
    { id: "throws", tier: "files", needsTarget: false, run: () => { throw new Error("boom"); } },
    { id: "network-only", tier: "network", needsTarget: false, run: () => ["from the registry"] },
  ];
  const result = await runAdmission({ root: "/app", config: {}, platform: "win", target: {}, rows, tiers: ["files", "host"] });
  assert.deepEqual(result.findings, [{ row: "fails", detail: "first problem" }, { row: "fails", detail: "second problem" }, { row: "throws", detail: "boom" }]);
  assert.deepEqual(result.rows.map((row) => [row.id, row.status]), [["passes", "ok"], ["fails", "fail"], ["cannot-run", "not-run"], ["not-applicable", "n/a"], ["elsewhere", "skipped"], ["throws", "fail"]]);
  assert.deepEqual(result.notRun, [{ id: "cannot-run", reason: "needs a Windows host" }]);
  assert.deepEqual(formatNotRun(result), ["NOT RUN cannot-run: needs a Windows host"]);
  const lines = formatAdmission(result);
  assert.equal(lines[0], "admission: 6 rows, 2 failed, 1 NOT RUN");
  assert.ok(lines.includes("  [NOT RUN] cannot-run: needs a Windows host"));
  assert.ok(lines.some((line) => /\[FAIL {2}\] fails: first problem/.test(line)));
});

test("a row can fail and also say that part of it could not run: the finding blocks, the NOT RUN is still listed", async () => {
  const rows = [{ id: "both", tier: "host", needsTarget: false, run: () => ({ findings: ["it failed"], notRun: "the rest could not run" }) }];
  const result = await runAdmission({ root: "/app", config: {}, platform: "win", target: {}, rows });
  assert.deepEqual(result.findings, [{ row: "both", detail: "it failed" }]);
  assert.deepEqual(result.rows.map((row) => row.status), ["fail"]);
  assert.deepEqual(result.notRun, [{ id: "both", reason: "the rest could not run" }]);
  assert.deepEqual(formatNotRun(result), ["NOT RUN both: the rest could not run"]);
  assert.equal(formatAdmission(result)[0], "admission: 1 rows, 1 failed, 1 NOT RUN");
  assert.ok(formatAdmission(result).includes("         NOT RUN: the rest could not run"));
});

test("a target that cannot be expanded is the `target` finding and blocks the rows that read it", async () => {
  const rows = [rowOf("package-command"), rowOf("config-identity")];
  const result = await runAdmission({ root: tempRoot("admission-unit-"), config: { schema: 1, app: "fixture", version: "1.0.0", targets: {} }, platform: "win", tiers: ["files"], rows });
  assert.deepEqual(result.findings, [{ row: "target", detail: "fixture has no win release target" }]);
  assert.deepEqual(result.rows.map((row) => [row.id, row.status]), [["target", "fail"], ["package-command", "blocked"], ["config-identity", "ok"]]);
});

test("rows the doctor reports as its own checks are skipped for it and run for the build", async () => {
  const rows = [{ id: "own", tier: "files", doctorOwned: true, needsTarget: false, run: () => ["owned"] }, { id: "shared", tier: "files", needsTarget: false, run: () => undefined }];
  assert.deepEqual((await runAdmission({ root: "/app", config: {}, platform: "win", target: {}, rows, skipDoctorOwned: true })).rows.map((row) => row.id), ["shared"]);
  assert.deepEqual((await runAdmission({ root: "/app", config: {}, platform: "win", target: {}, rows })).rows.map((row) => row.id), ["own", "shared"]);
});

// ---- files tier ----------------------------------------------------------------------------------------------------------------
test("config-identity: schema 1, an app and a version that are safe release-id components", async () => {
  assert.deepEqual((await unit("config-identity")).details, []);
  assert.deepEqual((await unit("config-identity", { config: { app: "fixture", version: "1.0.0" } })).details, ["unsupported config schema: <missing> (expected 1)"]);
  assert.deepEqual((await unit("config-identity", { config: { schema: 1, app: "fixture" } })).details, ["release config must expose app and version"]);
  assert.deepEqual((await unit("config-identity", { config: { schema: 1, app: "bad app", version: "1.0.0" } })).details, ["app and version must be safe release-id components"]);
  assert.deepEqual((await unit("config-identity", { config: { schema: 1, app: "fixture", version: "../1" } })).details, ["app and version must be safe release-id components"]);
});

test("package-command: a package command or a recipe that replaces it, except on android and ios", async () => {
  assert.deepEqual((await unit("package-command", { target: { package: { cmd: "node" } } })).details, []);
  assert.deepEqual((await unit("package-command", { target: {} })).details, ["fixture has no win package command"]);
  assert.deepEqual((await unit("package-command", { target: { winPackage: {} } })).details, []);
  assert.equal((await unit("package-command", { platform: "android", target: {} })).row.status, "n/a");
  assert.equal((await unit("package-command", { platform: "ios", target: {} })).row.status, "n/a");
});

test("nsis-upgrade-contract: a Windows NSIS target needs the custom template with the in-place upgrade policy", async () => {
  const root = tempRoot("admission-nsis-");
  const nsis = { signingContract: "windows-raw-exe-authenticode-before-nsis-v1" };
  const config = { schema: 1, app: "fixture", version: "1.0.0", targets: { win: nsis } };
  assert.match((await unit("nsis-upgrade-contract", { root, config, target: nsis })).details[0], /^missing base Tauri config: /);
  mkdirSync(path.join(root, "src-tauri"));
  writeFileSync(path.join(root, "src-tauri", "tauri.conf.json"), JSON.stringify({ bundle: { windows: { nsis: {} } } }));
  assert.match((await unit("nsis-upgrade-contract", { root, config, target: nsis })).details[0], /^Windows releases require a custom NSIS template/);
  writeFileSync(path.join(root, "src-tauri", "tauri.conf.json"), JSON.stringify({ bundle: { windows: { nsis: { template: "installer.nsi" } } } }));
  writeFileSync(path.join(root, "src-tauri", "installer.nsi"), "Page custom reinstall\n");
  assert.match((await unit("nsis-upgrade-contract", { root, config, target: nsis })).details[0], /^Windows NSIS template must declare the RightKit automatic in-place upgrade policy/);
  writeFileSync(path.join(root, "src-tauri", "installer.nsi"), "!define RIGHTKIT_AUTOMATIC_IN_PLACE_UPGRADE\n");
  assert.deepEqual((await unit("nsis-upgrade-contract", { root, config, target: nsis })).details, []);
  const portable = { signingContract: "windows-raw-exe-authenticode-before-portable-v1" };
  assert.equal((await unit("nsis-upgrade-contract", { root, config: { ...config, targets: { win: portable } }, target: portable })).row.status, "n/a");
  assert.equal((await unit("nsis-upgrade-contract", { root, config: { ...config, targets: { win: { prePackage: {} } } }, target: nsis })).row.status, "n/a", "a winPackage recipe declares no contract; the build does not hold it to this one");
  assert.equal((await unit("nsis-upgrade-contract", { root, config: { ...config, targets: { mac: nsis } }, platform: "mac", target: nsis })).row.status, "n/a");
});

test("win-updater-artifacts: the post-code-signing minisign needs updater.artifacts to sign (the desktop-app-template's win target has none)", async () => {
  const nsis = { signingContract: "windows-raw-exe-authenticode-before-nsis-v1" };
  assert.deepEqual((await unit("win-updater-artifacts", { target: { ...nsis, updater: { artifacts: [{ file: "a.exe" }] } } })).details, []);
  assert.deepEqual((await unit("win-updater-artifacts", { target: { ...nsis, updater: { artifacts: [] } } })).details, ["fixture win must declare updater.artifacts for post-code-signing minisign"]);
  assert.deepEqual((await unit("win-updater-artifacts", { target: nsis })).details, ["fixture win must declare updater.artifacts for post-code-signing minisign"]);
  assert.equal((await unit("win-updater-artifacts", { target: { signingContract: "windows-raw-exe-authenticode-before-portable-v1" } })).row.status, "n/a");
});

test("seal-routes: what the seal reads from the config, decided before anything is compiled or signed", async () => {
  const good = { artifacts: [], updater: { artifacts: [{ file: "a.exe", signature: "a.exe.sig", key: "app/updates/windows/current/a.exe" }] } };
  const config = (extra = {}) => ({ schema: 1, app: "fixture", version: "1.0.0", distribution: { provider: "r2-rightapps", updaterPublicKey: "RWQkey" }, ...extra });
  assert.deepEqual((await unit("seal-routes", { config: config(), target: good })).details, []);
  assert.deepEqual((await unit("seal-routes", { config: config(), target: { artifacts: [] } })).details, ["nothing to seal: declare installer.artifacts, updater.artifacts or artifacts for the win target"]);
  assert.deepEqual((await unit("seal-routes", { config: config(), target: { artifacts: [42] } })).details, ["target.artifacts entries must be paths or { file }"]);
  assert.deepEqual((await unit("seal-routes", { config: config({ distribution: { provider: "r2-rightapps" } }), target: good })).details, ["updater public key is required to verify updater artifacts before sealing"]);
  assert.deepEqual((await unit("seal-routes", { config: config({ distribution: { provider: "r2-rightapps" } }), target: { ...good, updater: { ...good.updater, pubkey: "RWQkey" } } })).details, []);
  // a Right Suite app declares no updater key: its trust root is the Right Suite contract's, in any spelling of the app name
  for (const app of ["mailright", "MailRight"]) {
    assert.deepEqual((await unit("seal-routes", { config: config({ app, distribution: { provider: "r2-rightapps" } }), target: good })).details, [], `${app} needs no declared updater key`);
  }
  assert.deepEqual((await unit("seal-routes", { config: config(), target: { updater: { artifacts: [{ file: "a.exe", key: "app/updates/windows/current/a.exe" }] } } })).details, ["updater artifact a.exe must declare the signature file sign-updater writes next to it"]);
  // the route key: required unless github-releases names it from the file; the TypeError the seal threw after compile and signing
  const keyless = { updater: { artifacts: [{ file: "a.exe", signature: "a.exe.sig" }] } };
  assert.deepEqual((await unit("seal-routes", { config: config(), target: keyless })).details, ["updater artifact a.exe must declare the R2 route key (key); only github-releases names the route from the file"]);
  assert.deepEqual((await unit("seal-routes", { config: config({ distribution: { provider: "github-releases", repository: "owner/name", updaterPublicKey: "RWQkey" } }), target: keyless })).details, []);
  // a private release is held to the R2 key shapes publish-update.mjs checks at upload, tier by tier: both tiers check the updater's
  // current key; only the patch tier checks the installer keys and where the patch updater lands, and it needs installer.artifacts
  const updaterOnly = { updater: { artifacts: [{ file: "a.exe", signature: "a.exe.sig", key: "app/a.exe" }] } };
  assert.deepEqual((await unit("seal-routes", { config: config(), target: updaterOnly })).details, ["updater key must target the replaceable current R2 object, got: app/a.exe"], "no installer.artifacts, no patch tier: only the update tier's key rule");
  assert.deepEqual((await unit("seal-routes", { config: config(), target: { updater: { artifacts: [{ ...updaterOnly.updater.artifacts[0], key: "app/updates/windows/current/a.exe", patchKey: "app/a.exe" }] } } })).details, [], "an update-only app is not held to the patch key");
  const odd = { ...updaterOnly, installer: { artifacts: [{ file: "i.exe", key: "app/i.exe", versionedKey: "app/i.exe" }] } };
  const shapes = (await unit("seal-routes", { config: config(), target: odd })).details;
  assert.deepEqual(shapes, [
    "installer key must target the replaceable current R2 object, got: app/i.exe",
    "installer versionedKey must be <app>/installers/<os>/versions/<version>/<file>, got: app/i.exe",
    "updater key must target the replaceable current R2 object, got: app/a.exe",
    "patch updater key must be convertible to installers/current, got: app/a.exe",
  ]);
  // the patch tier uploads the updater as an installer, under patchKey or the updater key's installers twin, and checks that key too
  const conforming = { installer: { artifacts: [{ file: "i.exe", key: "app/installers/windows/current/i.exe", versionedKey: "app/installers/windows/versions/1.0.0/i.exe" }] }, updater: { artifacts: [{ file: "a.exe", signature: "a.exe.sig", key: "app/updates/windows/current/a.exe" }] } };
  assert.deepEqual((await unit("seal-routes", { config: config(), target: conforming })).details, []);
  // an installer with no key: publish-update.mjs --tier patch tests the key as it is and refuses it with "got: undefined"
  const keylessInstaller = { ...conforming, installer: { artifacts: [{ file: "i.exe" }] } };
  assert.deepEqual((await unit("seal-routes", { config: config(), target: keylessInstaller })).details, ["installer key must target the replaceable current R2 object, got: undefined"]);
  assert.deepEqual((await unit("seal-routes", { config: config({ distribution: { provider: "github-releases", repository: "owner/name", updaterPublicKey: "RWQkey" } }), target: keylessInstaller })).details, [], "only a private release is held to the installer key");
  const strayPatch = { ...conforming, updater: { artifacts: [{ ...conforming.updater.artifacts[0], patchKey: "app/a.exe" }] } };
  assert.deepEqual((await unit("seal-routes", { config: config(), target: strayPatch })).details, ["installer key must target the replaceable current R2 object, got: app/a.exe"]);
  assert.deepEqual((await unit("seal-routes", { config: config(), target: { ...conforming, updater: { artifacts: [{ ...conforming.updater.artifacts[0], patchKey: "app/installers/mac/current/a.dmg" }] } } })).details, []);
  // only a private release is held to any of it
  assert.deepEqual((await unit("seal-routes", { config: config({ distribution: { provider: "github-releases", repository: "owner/name", updaterPublicKey: "RWQkey" } }), target: { ...odd, updater: { artifacts: [{ ...odd.updater.artifacts[0] }] } } })).details, []);
  assert.deepEqual((await unit("seal-routes", { config: config({ distribution: undefined }), target: { ...odd, updater: { artifacts: [{ ...odd.updater.artifacts[0], pubkey: "x" }], pubkey: "RWQkey" } } })).details, []);
  // android and ios pin one file of target.artifacts
  assert.deepEqual((await unit("seal-routes", { config: config(), platform: "ios", target: { artifacts: ["a.ipa"], recipe: { kind: "iosPackage", spec: { ipa: "other.ipa" } } } })).details, ["ios seal requires targets.ios.artifacts to list iosPackage.ipa"]);
  assert.deepEqual((await unit("seal-routes", { config: config(), platform: "ios", target: { artifacts: ["a.ipa"], recipe: { kind: "iosPackage", spec: { ipa: "./a.ipa" } } } })).details, []);
});

test("distribution-provider: the provider publish-update.mjs takes for a win or mac release, a missing distribution included", async () => {
  const withProvider = (distribution) => ({ schema: 1, app: "fixture", version: "1.0.0", distribution });
  const missing = "release config distribution.provider must be r2-rightapps or github-releases";
  // publish-update.mjs reads config.distribution?.provider and refuses what is not one of the two, undefined too
  for (const platform of ["win", "mac"]) {
    assert.deepEqual((await unit("distribution-provider", { platform, config: { schema: 1, app: "fixture", version: "1.0.0" } })).details, [missing], platform);
    assert.deepEqual((await unit("distribution-provider", { platform, config: withProvider(undefined) })).details, [missing], platform);
    assert.deepEqual((await unit("distribution-provider", { platform, config: withProvider({}) })).details, [missing], platform);
    assert.deepEqual((await unit("distribution-provider", { platform, config: withProvider({ provider: "r2-rightapps" }) })).details, [], platform);
  }
  assert.deepEqual((await unit("distribution-provider", { config: withProvider({ provider: "r2-postrights" }) })).details, ['release config distribution.provider must be r2-rightapps or github-releases (got "r2-postrights")']);
  assert.match((await unit("distribution-provider", { config: withProvider({ provider: "github-releases" }) })).details[0], /repository/i);
  assert.deepEqual((await unit("distribution-provider", { config: withProvider({ provider: "github-releases", repository: "bogusyogi/rightkit" }) })).details, []);
  // iOS and Android do not upload through publish-update.mjs (it takes win and mac): their providers are not its business
  for (const [platform, provider] of [["ios", "app-store-connect"], ["android", "google-play"], ["linux", "r2-rightapps"]]) {
    const outcome = await unit("distribution-provider", { platform, config: withProvider({ provider }) });
    assert.equal(outcome.row.status, "n/a", platform);
    assert.deepEqual(outcome.details, [], platform);
  }
  assert.equal((await unit("distribution-provider", { platform: "ios", config: { schema: 1, app: "fixture", version: "1.0.0" } })).row.status, "n/a");
});

test("input-key-collision: two inputs may not share a sealed hash key", async () => {
  const root = tempRoot("admission-keys-");
  const clash = path.join(root, "x", path.basename(root), "package.json");
  const inputs = (requiredInputs) => () => ({ ok: true, value: { requiredInputs, buildInputs: {} } });
  const result = await unit("input-key-collision", { root, resolveInputs: inputs([path.join(root, "package.json"), path.join(root, "pnpm-lock.yaml"), clash]) });
  assert.deepEqual(result.details, [`release inputs share the hash key ${path.join(path.basename(root), "package.json")}; rename one so every input is sealed`]);
  assert.deepEqual((await unit("input-key-collision", { root, resolveInputs: inputs([path.join(root, "package.json"), path.join(root, "pnpm-lock.yaml")]) })).details, []);
  assert.equal((await unit("input-key-collision", { root, resolveInputs: () => ({ ok: false, error: new Error("blocked") }) })).row.status, "skipped");
});

test("frontend-config-readable: the mock scan finds the frontend bundle through src-tauri/tauri.conf.json", async () => {
  const root = tempRoot("admission-frontend-");
  assert.equal((await unit("frontend-config-readable", { root })).row.status, "n/a");
  mkdirSync(path.join(root, "src-tauri"));
  writeFileSync(path.join(root, "src-tauri", "tauri.conf.json"), "{ not json");
  assert.match((await unit("frontend-config-readable", { root })).details[0], /^src-tauri\/tauri\.conf\.json is unreadable, so the frontend bundle cannot be located for the mock scan: /);
  assert.equal((await unit("frontend-config-readable", { root, target: { webBundleScan: ["dist"] } })).row.status, "n/a");
  writeFileSync(path.join(root, "src-tauri", "tauri.conf.json"), "{}");
  assert.deepEqual((await unit("frontend-config-readable", { root })).details, []);
});

test("declared-checks-exist: every config.checks entry is a script of the app's package.json", async () => {
  const root = tempRoot("admission-checks-");
  const config = (checks) => ({ schema: 1, app: "fixture", version: "1.0.0", checks });
  assert.equal((await unit("declared-checks-exist", { root, config: config([]) })).row.status, "n/a");
  assert.match((await unit("declared-checks-exist", { root, config: config(["lint"]) })).details[0], /^config\.checks declares "lint" but .*package\.json is missing or unreadable$/);
  writeFileSync(path.join(root, "package.json"), JSON.stringify({ scripts: { typecheck: "tsc" } }));
  assert.deepEqual((await unit("declared-checks-exist", { root, config: config(["typecheck"]) })).details, []);
  assert.deepEqual((await unit("declared-checks-exist", { root, config: config(["typecheck", "lint"]) })).details, ['config.checks names "lint", which is not a script in package.json']);
  assert.equal((await unit("declared-checks-exist", { root, config: config(["lint"]), skipChecks: true })).row.status, "n/a");
  const nested = { ...config(["lint"]), workdir: "web" };
  mkdirSync(path.join(root, "web"));
  writeFileSync(path.join(root, "web", "package.json"), JSON.stringify({ scripts: { lint: "eslint" } }));
  assert.deepEqual((await unit("declared-checks-exist", { root, config: nested })).details, []);
});

test("sidecar-app-entry and pre-seal-command", async () => {
  const mediaSidecar = { mediaSidecar: {} };
  assert.deepEqual((await unit("sidecar-app-entry", { platform: "mac", target: { ...mediaSidecar, sign: { prePackageFiles: ["dist/App.app"] } } })).details, []);
  assert.deepEqual((await unit("sidecar-app-entry", { platform: "mac", target: { ...mediaSidecar, sign: { prePackageFiles: ["dist/App.dmg"] } } })).details, ["mediaSidecar on macOS needs an .app entry in sign.prePackageFiles"]);
  assert.deepEqual((await unit("sidecar-app-entry", { platform: "mac", target: { externalBin: [], sign: { prePackageFiles: [] } } })).details, ["externalBin on macOS needs an .app entry in sign.prePackageFiles"]);
  assert.equal((await unit("sidecar-app-entry", { platform: "win", target: mediaSidecar })).row.status, "n/a");
  assert.deepEqual((await unit("pre-seal-command", { target: { preSeal: { cmd: "node" } } })).details, []);
  assert.deepEqual((await unit("pre-seal-command", { target: { preSeal: {} } })).details, ["release preSeal check must declare a command"]);
  assert.equal((await unit("pre-seal-command", { target: {} })).row.status, "n/a");
});

// ---- network tier --------------------------------------------------------------------------------------------------------------
test("rightkit-registry: the caller's resolver, its refusal worded as the doctor words it", async () => {
  const resolution = { snapshot: { digest: "d" } };
  const ok = await unit("rightkit-registry", { registry: async () => resolution });
  assert.deepEqual(ok.details, []);
  assert.equal(ok.result.out.registry, resolution);
  const stale = new RightKitRegistryError("stale-pin", "RightKit pins are behind the latest published version (build admission refused):\n- @rightkit/license pinned 0.1.0 -> latest 0.2.0");
  assert.deepEqual((await unit("rightkit-registry", { registry: async () => { throw stale; } })).details, [describeRegistryError(stale, "fixture")]);
  assert.match(describeRegistryError(stale, "fixture"), /^fixture RightKit pins are behind the latest published version/);
  const outage = new RightKitRegistryError("registry-outage", "registry unreachable", { recoverable: true });
  assert.equal(describeRegistryError(outage, "fixture"), "fixture RECOVERABLE registry unreachable");
  assert.equal(describeRegistryError(new TypeError("x"), "fixture"), "RightKit registry resolution threw: x");
  assert.equal((await unit("rightkit-registry", {})).row.status, "n/a");
});

// ---- host tier: fakes for the file system, the environment and the processes -------------------------------------------------------
// The rows build paths with the host's path module (path.join, path.resolve), so the fake file system keys its files the same way:
// a POSIX literal like "/app/package.json" and the path the row asks for are the same file on every host.
const at = (file) => path.resolve(file);
/** A fake file system over `files` (path to text) and `present` (paths that exist but hold nothing). */
function fakeFs({ files = {}, present = [] } = {}) {
  const texts = new Map(Object.entries(files).map(([file, text]) => [at(file), text]));
  const exists = new Set([...texts.keys(), ...present.map(at)]);
  return {
    existsSync: (file) => exists.has(at(file)),
    readFileSync: (file) => { if (!texts.has(at(file))) throw Object.assign(new Error(`ENOENT ${file}`), { code: "ENOENT" }); return texts.get(at(file)); },
    readdirSync: () => [],
  };
}
/** A POSIX host: `tools` are the executables on PATH, `answers` map "name args" to a spawn result, `files` are text files. */
function posixHost({ tools = ["pnpm", "cargo", "rustc"], answers = {}, files = {}, env = {}, hostPlatform = "linux" } = {}) {
  const calls = [];
  const fs = fakeFs({ files, present: tools.map((name) => `/bin/${name}`) });
  const spawn = (file, args = []) => {
    const key = `${path.basename(file)} ${args.join(" ")}`.trim();
    calls.push(key);
    const answer = answers[key];
    if (answer === undefined) return { status: 0, stdout: "", stderr: "" };
    return typeof answer === "function" ? answer() : answer;
  };
  return { fs, spawn, calls, env: { PATH: "/bin", ...env }, hostPlatform };
}
const ok = (stdout) => ({ status: 0, stdout, stderr: "" });
const RUSTC = (release) => ok(`rustc ${release} (fixture)\nbinary: rustc\nhost: x86_64-unknown-linux-gnu\nrelease: ${release}\n`);
const APP = { schema: 1, app: "fixture", version: "1.0.0", packageManager: "pnpm" };

test("host-tools: node, pnpm, cargo and rustc are present; pnpm is the packageManager pin; rustc is the rust-toolchain channel", async () => {
  const feed = { packageManager: "pnpm@11.25.0" };
  const healthy = posixHost({
    answers: { "pnpm --version": ok("11.25.0\n"), "cargo --version": ok("cargo 1.98.0"), "rustc -vV": RUSTC("1.98.0") },
    files: { "/app/package.json": JSON.stringify({ packageManager: "pnpm@11.25.0", engines: { node: ">=20" } }), "/app/rust-toolchain.toml": '[toolchain]\nchannel = "1.98.0"\n' },
  });
  assert.deepEqual((await unit("host-tools", { root: "/app", config: APP, target: {}, feed, ...healthy })).details, []);
  assert.deepEqual(healthy.calls, ["pnpm --version", "cargo --version", "rustc -vV"]);

  // a missing tool is the build's own text, and a missing tool is not probed
  const noCargo = posixHost({ tools: ["pnpm", "rustc"], answers: { "pnpm --version": ok("11.25.0"), "rustc -vV": RUSTC("1.98.0") }, files: { "/app/package.json": "{}" } });
  assert.deepEqual((await unit("host-tools", { root: "/app", config: APP, target: {}, feed, ...noCargo })).details, ["required executable not found: cargo"]);
  assert.ok(!noCargo.calls.includes("cargo --version"));

  // the pnpm on this host against the pin of the app's package.json
  const wrongPnpm = posixHost({ answers: { "pnpm --version": ok("11.24.0\n"), "cargo --version": ok("cargo"), "rustc -vV": RUSTC("1.98.0") }, files: { "/app/package.json": JSON.stringify({ packageManager: "pnpm@11.25.0" }) } });
  assert.deepEqual((await unit("host-tools", { root: "/app", config: APP, target: {}, feed, ...wrongPnpm })).details, [
    "pnpm 11.24.0 is installed but package.json packageManager pins pnpm@11.25.0; install the pinned version (corepack prepare pnpm@11.25.0 --activate)",
  ]);

  // the app's own pin against the feed's: the build says so before the worker, the doctor leaves it to its package rules
  const oldPin = posixHost({ answers: { "pnpm --version": ok("11.24.0"), "cargo --version": ok("cargo"), "rustc -vV": RUSTC("1.98.0") }, files: { "/app/package.json": JSON.stringify({ packageManager: "pnpm@11.24.0" }) } });
  assert.deepEqual((await unit("host-tools", { root: "/app", config: APP, target: {}, feed, ...oldPin })).details, ["fixture packageManager must be pnpm@11.25.0, got pnpm@11.24.0"]);
  assert.deepEqual((await unit("host-tools", { root: "/app", config: APP, target: {}, feed, mode: "doctor", ...oldPin })).details, []);

  // rustc against the channel rustup selects (a numeric channel only)
  const wrongRust = posixHost({ answers: { "pnpm --version": ok("11.25.0"), "cargo --version": ok("cargo"), "rustc -vV": RUSTC("1.97.0") }, files: { "/app/package.json": "{}", "/app/rust-toolchain.toml": '[toolchain]\nchannel = "1.98.0"\n' } });
  assert.deepEqual((await unit("host-tools", { root: "/app", config: APP, target: {}, feed, ...wrongRust })).details, ["rustc 1.97.0 is installed but rust-toolchain.toml selects 1.98.0; install it (rustup toolchain install 1.98.0)"]);
  const floating = posixHost({ answers: { "pnpm --version": ok("11.25.0"), "cargo --version": ok("cargo"), "rustc -vV": RUSTC("1.97.0") }, files: { "/app/package.json": "{}", "/app/rust-toolchain.toml": '[toolchain]\nchannel = "stable"\n' } });
  assert.deepEqual((await unit("host-tools", { root: "/app", config: APP, target: {}, feed, ...floating })).details, []);

  // a tool that is on PATH and fails is a finding with its output
  const broken = posixHost({ answers: { "pnpm --version": ok("11.25.0"), "cargo --version": { status: 101, stdout: "", stderr: "error: no default toolchain\n" }, "rustc -vV": RUSTC("1.98.0") }, files: { "/app/package.json": "{}" } });
  assert.deepEqual((await unit("host-tools", { root: "/app", config: APP, target: {}, feed, ...broken })).details, ["required command failed: cargo --version failed: error: no default toolchain"]);

  // an iOS app without a Cargo host needs neither cargo nor rustc
  const cargoless = posixHost({ tools: ["pnpm"], answers: { "pnpm --version": ok("11.25.0") }, files: { "/app/package.json": "{}" } });
  const iosApp = { ...APP, targets: { ios: { signed: true, iosPackage: { teamId: "T", bundleId: "b", project: "App.xcodeproj" } } } };
  assert.deepEqual((await unit("host-tools", { root: "/app", config: iosApp, platform: "ios", target: iosApp.targets.ios, feed, ...cargoless })).details, []);
});

test("host-secret-env: the signing secrets are not taken from the shell, whatever the host", async () => {
  const updater = { updater: { artifacts: [{ file: "a.exe" }] } };
  const managed = { PATH: "/home/u/.rightkit-managed/agent-bin:/bin" };
  // the acceptance case: TAURI_SIGNING_PRIVATE_KEY on a managed host
  const refused = await unit("host-secret-env", { target: updater, env: { ...managed, TAURI_SIGNING_PRIVATE_KEY: "value-7f3a1", TAURI_SIGNING_PRIVATE_KEY_PASSWORD: "value-c92b4" } });
  assert.deepEqual(refused.details, [new ManagedHostEnvSecretError("TAURI_SIGNING_PRIVATE_KEY").message, new ManagedHostEnvSecretError("TAURI_SIGNING_PRIVATE_KEY_PASSWORD").message]);
  assert.ok(!JSON.stringify(refused).includes("value-"), "the name is refused, the value is never printed");
  assert.deepEqual((await unit("host-secret-env", { target: updater, env: { RIGHTKIT_BUILD_BROKER_SOCKET: "/s", TAURI_SIGNING_PRIVATE_KEY: "x" } })).details, [new ManagedHostEnvSecretError("TAURI_SIGNING_PRIVATE_KEY").message]);
  // an unmanaged host refuses it too, in the words sign-updater.mjs uses
  assert.deepEqual((await unit("host-secret-env", { target: updater, env: { tauri_signing_private_key: "x" } })).details, ["updater signing key/password must come from the OS credential provider (tauri_signing_private_key is set in the environment; unset it)"]);
  assert.deepEqual((await unit("host-secret-env", { target: updater, env: { ...managed, TAURI_SIGNING_PRIVATE_KEY: "  " } })).details, []);
  assert.equal((await unit("host-secret-env", { target: {}, env: { ...managed, TAURI_SIGNING_PRIVATE_KEY: "x" } })).row.status, "n/a");
  assert.equal((await unit("host-secret-env", { target: { updater: { kind: "appinstaller", artifacts: [{ file: "a.appinstaller" }] } }, env: { TAURI_SIGNING_PRIVATE_KEY: "x" } })).row.status, "n/a");
  // the notary API variables are read only when no keychain profile is named, and refused on a managed host only
  const notarize = (profile) => ({ notarize: { file: "a.dmg", ...(profile ? { profile } : {}) } });
  const apple = { APPLE_API_KEY_PATH: "/k.p8", APPLE_API_KEY: "ID", APPLE_API_ISSUER: "iss" };
  assert.deepEqual((await unit("host-secret-env", { platform: "mac", target: notarize(), env: { ...managed, ...apple } })).details, ["APPLE_API_KEY_PATH", "APPLE_API_KEY", "APPLE_API_ISSUER"].map((name) => new ManagedHostEnvSecretError(name).message));
  assert.equal((await unit("host-secret-env", { platform: "mac", target: notarize("apple-dev-notary"), env: { ...managed, ...apple } })).row.status, "n/a");
  assert.deepEqual((await unit("host-secret-env", { platform: "mac", target: notarize(), env: apple })).details, []);
});

test("host-updater-credential: the provider that holds the updater key is on PATH of the Windows host that signs", async () => {
  const target = { updater: { artifacts: [{ file: "a.exe" }] } };
  const winHost = (tools) => ({ hostPlatform: "win32", env: { PATH: "C:\\bin", PATHEXT: ".EXE;.CMD" }, fs: { existsSync: (file) => tools.includes(file), readFileSync: () => "", readdirSync: () => [] }, spawn: () => ({ status: 0, stdout: "", stderr: "" }) });
  assert.deepEqual((await unit("host-updater-credential", { target, ...winHost(["C:\\bin\\rightkit-credentials.EXE"]) })).details, []);
  // off PATH: the text execWithUpdaterCredential raises when it cannot start the provider
  const absent = await unit("host-updater-credential", { target, ...winHost([]) });
  const raised = (() => { try { execWithUpdaterCredential(path.resolve("/x"), [], { env: {}, run: () => ({ error: { code: "ENOENT" } }) }); } catch (error) { return error.message; } })();
  assert.deepEqual(absent.details, [raised]);
  assert.match(raised, /^OS credential provider unavailable: install rightkit-credentials >=0\.2\.2 on PATH before updater signing$/);
  assert.equal((await unit("host-updater-credential", { target, hostPlatform: "linux", env: { PATH: "/bin" } })).row.status, "not-run");
  assert.equal((await unit("host-updater-credential", { target: {}, ...winHost([]) })).row.status, "n/a");
  assert.equal((await unit("host-updater-credential", { platform: "mac", target, ...winHost([]) })).row.status, "n/a");
});

test("host-signing-win: signtool, the Azure Artifact Signing dlib and the account that signs, read as sign-windows.mjs reads them", async () => {
  const nsis = { signingContract: "windows-raw-exe-authenticode-before-nsis-v1" };
  const host = ({ files = [], env = {}, user = {} } = {}) => ({
    hostPlatform: "win32",
    env,
    fs: { existsSync: (file) => files.includes(file), readFileSync: () => "", readdirSync: () => [] },
    // one PowerShell start answers every name, as name=value lines
    spawn: () => ({ status: 0, stdout: `${Object.entries(user).map(([name, value]) => `${name}=${value}`).join("\r\n")}\r\n`, stderr: "" }),
  });
  const configured = { AZURE_SIGNTOOL_PATH: "C:\\sdk\\signtool.exe", AZURE_CODESIGN_DLIB_PATH: "C:\\azure\\dlib.dll", AZURE_ARTIFACT_SIGNING_ENDPOINT: "https://e", AZURE_ARTIFACT_SIGNING_ACCOUNT: "a", AZURE_ARTIFACT_SIGNING_PROFILE: "p" };
  const files = ["C:\\sdk\\signtool.exe", "C:\\azure\\dlib.dll"];
  assert.deepEqual((await unit("host-signing-win", { target: nsis, ...host({ files, env: configured }) })).details, []);
  assert.deepEqual((await unit("host-signing-win", { target: nsis, ...host({ env: configured }) })).details, [
    "signtool.exe not found; set AZURE_SIGNTOOL_PATH or install Windows SDK Build Tools",
    "Azure.CodeSigning.Dlib.dll not found; set AZURE_CODESIGN_DLIB_PATH or install Azure Artifact Signing Client Tools",
  ]);
  assert.deepEqual((await unit("host-signing-win", { target: nsis, ...host({ files, env: { ...configured, AZURE_ARTIFACT_SIGNING_PROFILE: "" } }) })).details, ["set AZURE_ARTIFACT_SIGNING_ENDPOINT, AZURE_ARTIFACT_SIGNING_ACCOUNT, and AZURE_ARTIFACT_SIGNING_PROFILE"]);
  // the user-scope environment is read when the process environment does not have the name
  const { AZURE_ARTIFACT_SIGNING_PROFILE: profile, ...withoutProfile } = configured;
  assert.deepEqual((await unit("host-signing-win", { target: nsis, ...host({ files, env: withoutProfile, user: { AZURE_ARTIFACT_SIGNING_PROFILE: profile } }) })).details, []);
  // a metadata file replaces the three names, and must exist
  const { AZURE_ARTIFACT_SIGNING_ENDPOINT: e, AZURE_ARTIFACT_SIGNING_ACCOUNT: a, AZURE_ARTIFACT_SIGNING_PROFILE: p, ...base } = configured;
  assert.deepEqual((await unit("host-signing-win", { target: nsis, ...host({ files, env: { ...base, AZURE_ARTIFACT_SIGNING_METADATA: "C:\\m.json" } }) })).details, ["metadata file not found: C:\\m.json"]);
  assert.deepEqual((await unit("host-signing-win", { target: nsis, ...host({ files: [...files, "C:\\m.json"], env: { ...base, AZURE_ARTIFACT_SIGNING_METADATA: "C:\\m.json" } }) })).details, []);
  // not a Windows host: NOT RUN, never a pass
  assert.equal((await unit("host-signing-win", { target: nsis, hostPlatform: "darwin", env: {} })).row.status, "not-run");
  assert.equal((await unit("host-signing-win", { platform: "mac", target: nsis, ...host() })).row.status, "n/a");
});

test("host-signing-mac: one Developer ID identity and a notary profile the Keychain holds", async () => {
  const identity = '  1) AAAA "Developer ID Application: Fixture Inc (TEAM123456)"\n     1 valid identities found\n';
  const mac = (answers) => {
    const calls = [];
    return { hostPlatform: "darwin", env: { PATH: "/bin" }, calls, spawn: (file, args) => { calls.push(`${file} ${args.slice(0, 2).join(" ")}`); const key = `${file} ${args[0]}`; return answers[key] ?? answers[file] ?? { status: 0, stdout: "", stderr: "" }; } };
  };
  const target = { notarize: { file: "a.dmg" } };
  const healthy = mac({ "security find-identity": ok(identity), "security find-generic-password": ok("") });
  assert.deepEqual((await unit("host-signing-mac", { platform: "mac", target, ...healthy })).details, []);
  assert.ok(healthy.calls.some((call) => call.includes("com.apple.gke.notary.tool.saved-creds.apple-dev-notary") || call.startsWith("security find-generic-password")));
  assert.deepEqual((await unit("host-signing-mac", { platform: "mac", target, ...mac({ "security find-identity": ok("     0 valid identities found\n"), "security find-generic-password": ok("") }) })).details, ["no Developer ID Application signing identity found in Keychain"]);
  const two = identity.replace("     1 valid", '  2) BBBB "Developer ID Application: Other Inc (TEAM654321)"\n     2 valid');
  assert.match((await unit("host-signing-mac", { platform: "mac", target, ...mac({ "security find-identity": ok(two), "security find-generic-password": ok("") }) })).details[0], /^multiple Developer ID Application signing identities found in Keychain; set APPLE_DEVELOPER_ID explicitly/);
  // a profile the Keychain does not hold: notarytool is asked, and says so
  const missing = mac({ "security find-identity": ok(identity), "security find-generic-password": { status: 44, stdout: "", stderr: "" }, xcrun: { status: 1, stdout: "", stderr: "Error: No Keychain password item found for profile: apple-dev-notary" } });
  assert.deepEqual((await unit("host-signing-mac", { platform: "mac", target, ...missing })).details, ["notarytool keychain profile apple-dev-notary is not stored in the Keychain; store it once with: xcrun notarytool store-credentials apple-dev-notary"]);
  // notarytool cannot be asked (offline): NOT RUN, never a pass
  const offline = mac({ "security find-identity": ok(identity), "security find-generic-password": { status: 44, stdout: "", stderr: "" }, xcrun: { status: 1, stdout: "", stderr: "Error: The Internet connection appears to be offline" } });
  const inconclusive = await unit("host-signing-mac", { platform: "mac", target, ...offline });
  assert.equal(inconclusive.row.status, "not-run");
  assert.match(inconclusive.row.detail, /notarytool could not be asked about keychain profile apple-dev-notary/);
  // a named profile is the one probed; API variables alone are the notary-auth.mjs rule
  const named = mac({ "security find-identity": ok(identity), "security find-generic-password": ok("") });
  assert.deepEqual((await unit("host-signing-mac", { platform: "mac", target: { notarize: { file: "a.dmg", profile: "team" } }, ...named })).details, []);
  assert.ok(named.calls.length > 0);
  const partial = { ...mac({ "security find-identity": ok(identity) }), env: { PATH: "/bin", APPLE_API_KEY: "ID" } };
  assert.deepEqual((await unit("host-signing-mac", { platform: "mac", target, ...partial })).details, ["Notarization API auth requires APPLE_API_KEY_PATH, APPLE_API_KEY, and APPLE_API_ISSUER together"]);
  assert.equal((await unit("host-signing-mac", { platform: "mac", target, hostPlatform: "linux", env: {} })).row.status, "not-run");
  // how the profile is chosen needs no Mac: a half-set API trio is refused wherever the doctor runs
  assert.deepEqual((await unit("host-signing-mac", { platform: "mac", target, hostPlatform: "linux", env: { APPLE_API_KEY: "ID" } })).details, ["Notarization API auth requires APPLE_API_KEY_PATH, APPLE_API_KEY, and APPLE_API_ISSUER together"]);
  assert.equal((await unit("host-signing-mac", { platform: "win", target, ...healthy })).row.status, "n/a");
});

test("host-preflight-env: the environment, executables and commands the target declares as host preconditions", async () => {
  const host = posixHost({ tools: ["pnpm", "cargo", "rustc", "protoc"], answers: { "protoc --version": { status: 1, stdout: "", stderr: "" } }, env: { PRESENT: "1" } });
  const preflight = { env: ["PRESENT", "ABSENT"], executables: ["protoc", "flatc"], commands: [{ cmd: "protoc", args: ["--version"] }] };
  assert.deepEqual((await unit("host-preflight-env", { root: "/app", target: { preflight }, ...host })).details, [
    "required executable not found: flatc",
    "missing required environment variable: ABSENT",
    "protoc exited 1",
  ]);
  assert.equal((await unit("host-preflight-env", { root: "/app", target: {}, ...host })).row.status, "n/a");
  assert.deepEqual((await unit("host-preflight-env", { root: "/app", target: { preflight: { env: ["PRESENT"] } }, ...host })).details, []);
});

test("host-cache-mode: sccache >= 0.15.0 exactly when the build would use the shared cache", async () => {
  const sccache = (version) => posixHost({ tools: ["sccache"], answers: { "sccache --version": ok(version) } });
  // the build defaults to the shared cache on mac and win
  assert.deepEqual((await unit("host-cache-mode", { platform: "mac", target: {}, config: APP, ...sccache("sccache 0.15.0\n") })).details, []);
  const refusal = (found) => `shared cache mode requires sccache >= 0.15.0${found ? ` (found ${found})` : ""}. Install with: cargo install sccache --locked; then verify: sccache --version`;
  assert.deepEqual((await unit("host-cache-mode", { platform: "mac", target: {}, config: APP, ...sccache("sccache 0.14.2\n") })).details, [refusal("0.14.2")]);
  assert.deepEqual((await unit("host-cache-mode", { platform: "win", target: {}, config: APP, ...posixHost({ tools: [] }) })).details, [refusal(null)]);
  // an explicit legacy mode, CI, and a broker-managed host need none
  assert.equal((await unit("host-cache-mode", { platform: "mac", target: {}, config: APP, ...posixHost({ tools: [], env: { RIGHT_RELEASE_CACHE_MODE: "legacy" } }) })).row.status, "n/a");
  assert.equal((await unit("host-cache-mode", { platform: "mac", target: {}, config: APP, ...posixHost({ tools: [], env: { GITHUB_ACTIONS: "true" } }) })).row.status, "n/a");
  assert.equal((await unit("host-cache-mode", { platform: "mac", target: {}, config: APP, ...posixHost({ tools: [], env: { RIGHTKIT_BUILD_BROKER_SOCKET: "/s" }, hostPlatform: "darwin" }) })).row.status, "n/a");
  // Linux is never broker-managed (managed-cargo directPlatform), whatever socket the environment names: the shared cache still needs sccache
  assert.deepEqual((await unit("host-cache-mode", { platform: "mac", target: {}, config: APP, ...posixHost({ tools: [], env: { RIGHTKIT_BUILD_BROKER_SOCKET: "/s" }, hostPlatform: "linux" }) })).details, [refusal(null)]);
  assert.deepEqual((await unit("host-cache-mode", { platform: "mac", target: {}, config: APP, ...posixHost({ tools: [], env: { RIGHT_RELEASE_CACHE_MODE: "turbo" } }) })).details, ["RIGHT_RELEASE_CACHE_MODE must be legacy or shared"]);
  assert.equal((await unit("host-cache-mode", { platform: "linux", target: {}, config: APP, ...posixHost({ tools: [] }) })).row.status, "n/a");
});

test("declared-checks-run: the doctor runs lint and typecheck when installed, says NOT RUN when not, and the build leaves them to its own step", async () => {
  const config = { ...APP, checks: ["typecheck", "lint"] };
  const calls = [];
  const appDir = path.resolve("/app");
  const host = (installed, fail = []) => ({
    root: "/app",
    config,
    env: { PATH: "/bin" },
    fs: fakeFs({ present: ["/bin/pnpm", ...(installed ? [path.join(appDir, "node_modules")] : [])] }),
    spawn: (file, args, options) => {
      calls.push([path.basename(file), ...args, options.cwd]);
      return fail.includes(args[1]) ? { status: 2, stdout: "src/a.ts(1,1): error TS2322: nope\n", stderr: "" } : { status: 0, stdout: "", stderr: "" };
    },
    hostPlatform: "linux",
  });
  const none = await unit("declared-checks-run", { ...host(false), mode: "doctor", target: {} });
  assert.equal(none.row.status, "not-run");
  assert.ok(none.row.detail.startsWith(`not installed: no node_modules in ${appDir}`), none.row.detail);
  assert.deepEqual(calls, []);
  const clean = await unit("declared-checks-run", { ...host(true), mode: "doctor", target: {} });
  assert.deepEqual(clean.details, []);
  assert.deepEqual(calls, [["pnpm", "run", "typecheck", appDir], ["pnpm", "run", "lint", appDir]]);
  calls.length = 0;
  const red = await unit("declared-checks-run", { ...host(true, ["typecheck"]), mode: "doctor", target: {} });
  assert.equal(red.details.length, 1);
  assert.match(red.details[0], /^check "typecheck" failed \(pnpm run typecheck exited 2\):\nsrc\/a\.ts\(1,1\): error TS2322: nope$/);
  assert.deepEqual(calls.map((call) => call[2]), ["typecheck", "lint"], "one failing check does not hide the next");
  calls.length = 0;
  assert.equal((await unit("declared-checks-run", { ...host(true), mode: "build", target: {} })).row.status, "skipped");
  assert.equal((await unit("declared-checks-run", { ...host(true), mode: "doctor", skipChecks: true, target: {} })).row.status, "n/a");
  assert.deepEqual(calls, []);
});

// A spawnSync over a command that takes `seconds` of virtual time and writes `bytes`, honouring `timeout` and `maxBuffer` the way Node does:
// a command past the timeout or the buffer is stopped and the result carries the error Node gives (ETIMEDOUT, ENOBUFS) and no exit status.
function runningFor({ seconds = 0, bytes = 0, status = 0, stdout = "" }, seen = []) {
  return (file, args, options) => {
    seen.push({ command: `${path.basename(file)} ${args.join(" ")}`, timeout: options.timeout, maxBuffer: options.maxBuffer });
    if (options.timeout && seconds * 1000 > options.timeout) return { status: null, signal: "SIGTERM", error: Object.assign(new Error("spawnSync pnpm ETIMEDOUT"), { code: "ETIMEDOUT" }), stdout: "", stderr: "" };
    if (bytes > (options.maxBuffer ?? 1024 * 1024)) return { status: null, signal: "SIGTERM", error: Object.assign(new Error("spawnSync pnpm ENOBUFS"), { code: "ENOBUFS" }), stdout: "", stderr: "" };
    return { status, stdout, stderr: "" };
  };
}
const MiB = 1024 * 1024;

test("declared-checks-run: a slow or noisy check that passes is green, one that exits non-zero is the finding, and one this tool had to stop is NOT RUN", async () => {
  const appDir = path.resolve("/app");
  const check = (command, checks = ["lint"], seen = []) => unit("declared-checks-run", {
    root: "/app", config: { ...APP, checks }, env: { PATH: "/bin" }, mode: "doctor", target: {}, hostPlatform: "linux",
    fs: fakeFs({ present: ["/bin/pnpm", path.join(appDir, "node_modules")] }), spawn: runningFor(command, seen),
  });
  // release.mjs runPackageScript starts the script with no time limit and inherited output: a lint that runs for ninety seconds (the
  // version probes' sixty-second cap) or writes three megabytes (spawnSync's one-megabyte default buffer) and exits 0 is green
  const seen = [];
  const slow = await check({ seconds: 90 }, ["lint"], seen);
  assert.deepEqual(slow.details, [], "ninety seconds is not a failed check");
  assert.equal(slow.row.status, "ok");
  assert.equal(seen[0].timeout, 4 * 60 * 60 * 1000, "a declared check is held to the build's absolute limit for a release step, not to the version probes' sixty seconds");
  assert.ok(seen[0].maxBuffer >= 256 * MiB, "and gets a buffer for the output of a real lint or test run");
  assert.deepEqual((await check({ seconds: 3600, bytes: 3 * MiB })).details, [], "an hour of lint is green");
  // a check that exits 1 is the finding, with the exit status and the end of its output
  const red = await check({ status: 1, stdout: "eslint: 3 problems\n" });
  assert.deepEqual(red.details, ['check "lint" failed (pnpm run lint exited 1):\neslint: 3 problems']);
  assert.equal(red.row.status, "fail");
  // a check stopped by this tool's own buffer or clock did not fail: NOT RUN, with the reason and what to do, never a finding
  const flood = await check({ bytes: 300 * MiB });
  assert.deepEqual(flood.details, []);
  assert.equal(flood.row.status, "not-run");
  assert.match(flood.row.detail, /^check "lint" wrote more than 256 MiB of output, more than this tool captures, and was stopped before it finished; the build runs it to its end, so run it yourself: pnpm run lint$/);
  // past the build's absolute limit the build stops the step too; the doctor cannot run it to its end, so it says NOT RUN, with the limit
  const runaway = await unit("declared-checks-run", {
    root: "/app", config: { ...APP, checks: ["lint"] }, env: { PATH: "/bin" }, mode: "doctor", target: {}, hostPlatform: "linux",
    fs: fakeFs({ present: ["/bin/pnpm", path.join(appDir, "node_modules")] }), spawn: runningFor({ seconds: 5 * 3600 }),
  });
  assert.equal(runaway.row.status, "not-run");
  assert.match(runaway.row.detail, /^check "lint" ran past 240 minutes, the build's absolute limit for a release step \(RIGHT_RELEASE_ABSOLUTE_MS\), and was stopped; the build runs it to its end, so run it yourself: pnpm run lint$/);
  // the limit is the one the build reads, so the same environment variable moves both
  const seenLimit = [];
  await unit("declared-checks-run", {
    root: "/app", config: { ...APP, checks: ["lint"] }, env: { PATH: "/bin", RIGHT_RELEASE_ABSOLUTE_MS: String(90 * 60 * 1000) }, mode: "doctor", target: {}, hostPlatform: "linux",
    fs: fakeFs({ present: ["/bin/pnpm", path.join(appDir, "node_modules")] }), spawn: runningFor({}, seenLimit),
  });
  assert.equal(seenLimit[0].timeout, 90 * 60 * 1000);
  // a small limit is said in seconds: Math.round(20000 / 60000) minutes would read "ran past 0 minutes"
  const stoppedAt = async (absoluteMs) => (await unit("declared-checks-run", {
    root: "/app", config: { ...APP, checks: ["lint"] }, env: { PATH: "/bin", RIGHT_RELEASE_STALL_MS: "500", RIGHT_RELEASE_ABSOLUTE_MS: String(absoluteMs) }, mode: "doctor", target: {}, hostPlatform: "linux",
    fs: fakeFs({ present: ["/bin/pnpm", path.join(appDir, "node_modules")] }), spawn: runningFor({ seconds: 3600 }),
  })).row.detail;
  assert.match(await stoppedAt(20_000), /^check "lint" ran past 20 seconds, the build's absolute limit/);
  assert.match(await stoppedAt(1000), /^check "lint" ran past 1 second, the build's absolute limit/);
  assert.match(await stoppedAt(90_000), /^check "lint" ran past 90 seconds, the build's absolute limit/);
  assert.match(await stoppedAt(60_000), /^check "lint" ran past 1 minute, the build's absolute limit/);
  assert.doesNotMatch(await stoppedAt(20_000), /0 minutes/);
  // a failed check next to one that could not run: the row fails, and the other is still said NOT RUN
  const mixed = await unit("declared-checks-run", {
    root: "/app", config: { ...APP, checks: ["typecheck", "lint"] }, env: { PATH: "/bin" }, mode: "doctor", target: {}, hostPlatform: "linux",
    fs: fakeFs({ present: ["/bin/pnpm", path.join(appDir, "node_modules")] }),
    spawn: (file, args, options) => (args[1] === "typecheck" ? runningFor({ status: 2, stdout: "TS2322\n" })(file, args, options) : runningFor({ bytes: 300 * MiB })(file, args, options)),
  });
  assert.equal(mixed.row.status, "fail");
  assert.deepEqual(mixed.details, ['check "typecheck" failed (pnpm run typecheck exited 2):\nTS2322']);
  assert.deepEqual(mixed.result.notRun.map((row) => row.id), ["declared-checks-run"]);
  assert.match(mixed.result.notRun[0].reason, /^check "lint" wrote more than 256 MiB/);
  // the version probes keep their sixty-second cap
  const probes = [];
  const healthy = posixHost({ answers: { "pnpm --version": ok("11.25.0"), "cargo --version": ok("cargo"), "rustc -vV": RUSTC("1.98.0") }, files: { "/app/package.json": JSON.stringify({ packageManager: "pnpm@11.25.0" }) } });
  await unit("host-tools", { root: "/app", config: APP, target: {}, feed: { packageManager: "pnpm@11.25.0" }, ...healthy, spawn: (file, args, options) => { probes.push(options.timeout); return healthy.spawn(file, args, options); } });
  assert.deepEqual(probes, [60_000, 60_000, 60_000]);
});

test("declared-checks-run with a real process: a check that writes more than spawnSync's default buffer and exits 0 is green", async () => {
  const dir = tempRoot("admission-real-check-");
  const bin = path.join(dir, "bin");
  mkdirSync(bin);
  // pnpm run <script> writes three megabytes to each stream and exits with the status the script name asks for
  writeFileSync(path.join(bin, "pnpm.js"), 'const big = "x".repeat(3 * 1024 * 1024); process.stdout.write(big); process.stderr.write(big); process.exitCode = process.argv[3] === "failing" ? 1 : 0;\n');
  if (process.platform === "win32") writeFileSync(path.join(bin, "pnpm.cmd"), '@echo off\r\nnode "%~dp0pnpm.js" %*\r\n');
  else writeFileSync(path.join(bin, "pnpm"), '#!/bin/sh\nexec node "$(dirname "$0")/pnpm.js" "$@"\n', { mode: 0o755 });
  writeFileSync(path.join(dir, "package.json"), JSON.stringify({ scripts: { lint: "x", failing: "x" } }));
  mkdirSync(path.join(dir, "node_modules"));
  // One PATH variable only: on Windows process.env can carry both "Path" and "PATH", and a second spelling left in place lets the
  // real pnpm win over the shim.
  const pathKey = process.platform === "win32" ? "Path" : "PATH";
  const inherited = Object.fromEntries(Object.entries(process.env).filter(([key]) => !/^path$/i.test(key)));
  const upstream = Object.entries(process.env).find(([key]) => /^path$/i.test(key))?.[1] ?? "";
  const run = (checks) => unit("declared-checks-run", {
    root: dir, config: { ...APP, checks }, mode: "doctor", target: {}, spawn: spawnSync, hostPlatform: process.platform,
    env: { ...inherited, [pathKey]: `${bin}${path.delimiter}${upstream}` },
  });
  const green = await run(["lint"]);
  assert.deepEqual(green.details, []);
  assert.equal(green.row.status, "ok");
  const red = await run(["failing"]);
  assert.equal(red.details.length, 1);
  assert.match(red.details[0], /^check "failing" failed \(pnpm run failing exited 1\):\n/);
});

test("host-preflight-env: a preflight command is captured with a large buffer, and one this tool had to stop is NOT RUN, not a failure", async () => {
  const preflight = { commands: [{ cmd: "protoc", args: ["--version"] }] };
  const seen = [];
  const host = (command) => ({ ...posixHost({ tools: ["protoc"] }), spawn: runningFor(command, seen) });
  assert.deepEqual((await unit("host-preflight-env", { root: "/app", target: { preflight }, ...host({ bytes: 3 * MiB }) })).details, [], "build-release.mjs runChecked inherits the output: three megabytes is not a failure");
  assert.ok(seen[0].maxBuffer >= 256 * MiB);
  assert.equal(seen[0].timeout, undefined, "the build sets no time limit on a preflight command");
  const flood = await unit("host-preflight-env", { root: "/app", target: { preflight }, ...host({ bytes: 300 * MiB }) });
  assert.equal(flood.row.status, "not-run");
  assert.match(flood.row.detail, /^preflight command protoc wrote more than 256 MiB of output/);
  assert.deepEqual((await unit("host-preflight-env", { root: "/app", target: { preflight }, ...host({ status: 3 }) })).details, ["protoc exited 3"]);
});

// ---- the mediaSidecar plan: the committed-files part and the cargo part --------------------------------------------------------------
const hex = (char) => char.repeat(64);
const sidecarRow = (tool, overrides = {}) => ({
  id: `${tool}-7.1.5-windows-x86_64`, tool, variant: "decode", os: "windows", arch: "x86_64", version: "7.1.5", status: "approved", sha256: hex("1"), contentSha256: hex("2"), sourceSha256: hex("3"),
  attribution: { licenseSpdx: "LGPL-2.1-or-later", sourceUrl: "https://ffmpeg.org/releases/ffmpeg-7.1.5.tar.xz", buildProvenance: "third-party/BUILD-NOTES.txt", noticeFiles: ["third-party/LGPL-2.1.txt"] },
  ...overrides,
});
const sidecarCatalog = (artifacts) => JSON.stringify({ schemaVersion: 2, artifacts });
const sidecarConfig = (extra = {}) => ({ arch: "x86_64", notices: { dir: "src-tauri/resources/ffmpeg", sourceOfferContact: "legal@example.test" }, ...extra });
const withLayout = (manifestPath) => ({ resolveInputs: () => ({ ok: true, value: { nativeLayout: manifestPath ? { manifestPath } : null, requiredInputs: [], buildInputs: {} } }) });

test("media-sidecar-plan: the app has a Cargo manifest to read the catalog from, and the catalog the config names plans", async () => {
  const root = tempRoot("admission-sidecar-");
  const app = { schema: 1, app: "fixture", version: "1.0.0" };
  const manifest = path.join(root, "src-tauri", "Cargo.toml");
  const catalogDir = path.join(root, "vendor", "ffmpeg", "catalog");
  mkdirSync(catalogDir, { recursive: true });
  const named = { mediaSidecar: sidecarConfig({ catalog: "vendor/ffmpeg/catalog/artifacts.json" }) };
  // no native Cargo manifest: the worker's text, before any compile
  assert.deepEqual((await unit("media-sidecar-plan", { root, config: app, target: named, ...withLayout(null) })).details, ["fixture win mediaSidecar needs a native Cargo manifest to read the sidecar catalog, and this app has none"]);
  // a catalog that cannot be read, has no row for a tool, or has a row that is not approved: the plan's own refusal, in the worker's wording
  const missing = (await unit("media-sidecar-plan", { root, config: app, target: named, ...withLayout(manifest) })).details;
  assert.equal(missing.length, 1);
  assert.match(missing[0], /^fixture win mediaSidecar: \[MEDIA_SIDECAR_CATALOG_INVALID\] cannot read /);
  writeFileSync(path.join(catalogDir, "artifacts.json"), sidecarCatalog([sidecarRow("ffmpeg")]));
  assert.deepEqual((await unit("media-sidecar-plan", { root, config: app, target: named, ...withLayout(manifest) })).details, ["fixture win mediaSidecar: [MEDIA_SIDECAR_NO_ROW] the rightkit-ffmpeg catalog has no ffprobe decode windows/x86_64 row"]);
  writeFileSync(path.join(catalogDir, "artifacts.json"), sidecarCatalog([sidecarRow("ffmpeg"), sidecarRow("ffprobe", { status: "pending" })]));
  assert.match((await unit("media-sidecar-plan", { root, config: app, target: named, ...withLayout(manifest) })).details[0], /^fixture win mediaSidecar: \[MEDIA_SIDECAR_NOT_APPROVED\] the ffprobe decode windows\/x86_64 row ffprobe-7\.1\.5-windows-x86_64 is pending, not approved$/);
  writeFileSync(path.join(catalogDir, "artifacts.json"), sidecarCatalog([sidecarRow("ffmpeg"), sidecarRow("ffprobe")]));
  assert.deepEqual((await unit("media-sidecar-plan", { root, config: app, target: named, ...withLayout(manifest) })).details, []);
  // without a catalog in the config the catalog comes from cargo metadata, which is the host row's; and nothing to plan, nothing to say
  assert.equal((await unit("media-sidecar-plan", { root, config: app, target: { mediaSidecar: sidecarConfig() }, ...withLayout(manifest) })).row.status, "skipped");
  assert.equal((await unit("media-sidecar-plan", { root, config: app, target: {}, ...withLayout(manifest) })).row.status, "n/a");
  assert.equal((await unit("media-sidecar-plan", { root, config: app, platform: "linux", target: named, ...withLayout(manifest) })).row.status, "n/a");
  // an invalid mediaSidecar configuration is the media-sidecar rule's finding; build inputs that failed are the build-inputs row's
  assert.equal((await unit("media-sidecar-plan", { root, config: app, target: { mediaSidecar: { nope: 1 } }, ...withLayout(manifest) })).row.status, "skipped");
  assert.equal((await unit("media-sidecar-plan", { root, config: app, target: named, resolveInputs: () => ({ ok: false, error: new Error("blocked") }) })).row.status, "skipped");
});

test("media-sidecar-catalog: with no catalog in the config, the rightkit-ffmpeg crate cargo metadata finds is planned, through the managed cargo the worker uses", async () => {
  const root = tempRoot("admission-sidecar-cargo-");
  const app = { schema: 1, app: "fixture", version: "1.0.0" };
  const manifest = path.join(root, "src-tauri", "Cargo.toml");
  const crateDir = path.join(root, "registry", "rightkit-ffmpeg-0.2.6");
  mkdirSync(path.join(crateDir, "catalog"), { recursive: true });
  const metadata = (packages) => ({ status: 0, stdout: JSON.stringify({ packages }), stderr: "" });
  const crate = { name: "rightkit-ffmpeg", version: "0.2.6", manifest_path: path.join(crateDir, "Cargo.toml") };
  const cargo = (answer) => {
    const calls = [];
    // no managed runtime client exists on this fake host, so the call is a plain `cargo`, whatever machine runs the test; the catalog is read for real
    return { calls, hostPlatform: "linux", env: { PATH: "/bin" }, fs: { existsSync: () => false, readFileSync, readdirSync: () => [] }, spawn: (file, args, options) => { calls.push({ file: path.basename(file), args, cwd: options.cwd }); return typeof answer === "function" ? answer() : answer; } };
  };
  const target = { mediaSidecar: sidecarConfig() };
  writeFileSync(path.join(crateDir, "catalog", "artifacts.json"), sidecarCatalog([sidecarRow("ffmpeg"), sidecarRow("ffprobe")]));
  const healthy = cargo(metadata([crate]));
  assert.deepEqual((await unit("media-sidecar-catalog", { root, config: app, target, ...healthy, ...withLayout(manifest) })).details, []);
  assert.deepEqual(healthy.calls, [{ file: "cargo", args: ["metadata", "--frozen", "--format-version", "1", "--manifest-path", manifest], cwd: path.dirname(manifest) }]);
  // the worker's refusals, in its wording: no crate in the dependency graph, cargo failing, cargo that cannot start
  assert.deepEqual((await unit("media-sidecar-catalog", { root, config: app, target, ...cargo(metadata([])), ...withLayout(manifest) })).details, ["fixture win mediaSidecar: [MEDIA_SIDECAR_CRATE_NOT_FOUND] the app's dependency graph has 0 rightkit-ffmpeg packages (expected exactly one)"]);
  assert.deepEqual((await unit("media-sidecar-catalog", { root, config: app, target, ...cargo({ status: 101, stdout: "", stderr: "error: no lock\n" }), ...withLayout(manifest) })).details, ["fixture win mediaSidecar: [MEDIA_SIDECAR_CRATE_NOT_FOUND] cargo metadata failed: error: no lock"]);
  assert.match((await unit("media-sidecar-catalog", { root, config: app, target, ...cargo({ error: new Error("spawn cargo ENOENT"), status: null }), ...withLayout(manifest) })).details[0], /^fixture win mediaSidecar: \[MEDIA_SIDECAR_CRATE_NOT_FOUND\] could not run cargo metadata: spawn cargo ENOENT$/);
  writeFileSync(path.join(crateDir, "catalog", "artifacts.json"), sidecarCatalog([sidecarRow("ffmpeg")]));
  assert.match((await unit("media-sidecar-catalog", { root, config: app, target, ...cargo(metadata([crate])), ...withLayout(manifest) })).details[0], /^fixture win mediaSidecar: \[MEDIA_SIDECAR_NO_ROW\] /);
  // a catalog the config names is the files row's; no layout is the files row's finding; nothing declared is nothing to ask
  const never = cargo(() => { throw new Error("cargo must not be asked"); });
  assert.equal((await unit("media-sidecar-catalog", { root, config: app, target: { mediaSidecar: sidecarConfig({ catalog: "c.json" }) }, ...never, ...withLayout(manifest) })).row.status, "n/a");
  assert.equal((await unit("media-sidecar-catalog", { root, config: app, target, ...never, ...withLayout(null) })).row.status, "skipped");
  assert.equal((await unit("media-sidecar-catalog", { root, config: app, target: {}, ...never, ...withLayout(manifest) })).row.status, "n/a");
  assert.equal((await unit("media-sidecar-catalog", { root, config: app, target: { mediaSidecar: { nope: 1 } }, ...never, ...withLayout(manifest) })).row.status, "skipped");
  assert.deepEqual(never.calls, []);
});
