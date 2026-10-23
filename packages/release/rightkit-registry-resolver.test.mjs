import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { assertPublishedRightKitCargoDependencies } from "./cargo-contract.mjs";
import {
  RightKitRegistryError, SNAPSHOT_FILE, assertManifestRegistryEvidence, evaluateRightKitFreshness, loadAdmittedSnapshot, loadSnapshot,
  prepareRightKitResolution, registryManifestFields, resolveRightKitPins, rightKitLockInventory, snapshotDigest, verifiedVersionMaps, writeAdmissionRecord,
} from "./rightkit-registry-resolver.mjs";
import { rightKitVersionAllowed } from "./rightkit-version-policy.mjs";

const SOURCE = "registry+https://github.com/rust-lang/crates.io-index";
const CK = (n) => String(n).repeat(64).slice(0, 64);
const INTEGRITY = (n) => `sha512-${String(n).repeat(8)}==`;
const NO_SLEEP = async () => {};
const bundled = {
  schema: 2,
  npm: { "@rightkit/bar": "0.4.1" },
  stagedNpm: {},
  legacyNpm: {},
  cargo: { "rightkit-foo": "0.1.0" },
  stagedCargo: {},
  legacyCargo: {},
  registry: { npm: { "@rightkit/bar": "0.4.1" }, cargo: { "rightkit-foo": "0.1.0" } },
};
const ownership = { capabilities: [{ package: "rightkit-foo" }, { package: "@rightkit/bar" }] };

function tempDir(t) {
  const dir = mkdtempSync(path.join(os.tmpdir(), "rk-resolver-"));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  return dir;
}
function cargoLock(packages) {
  return `version = 4\n${packages.map((p) => `\n[[package]]\nname = "${p.name}"\nversion = "${p.version}"\n${p.source === null ? "" : `source = "${p.source ?? SOURCE}"\n`}${p.checksum ? `checksum = "${p.checksum}"\n` : ""}`).join("")}`;
}
function pnpmLock(packages) {
  return `lockfileVersion: '9.0'\n\npackages:\n\n${packages.map((p) => `  '${p.name}@${p.version}':\n    resolution: {${p.resolution ?? `integrity: ${p.integrity}`}}\n`).join("\n")}`;
}
function app(t, { cargo = [], npm = [] }) {
  const root = tempDir(t);
  if (cargo.length) writeFileSync(path.join(root, "Cargo.lock"), cargoLock(cargo));
  if (npm.length) writeFileSync(path.join(root, "pnpm-lock.yaml"), pnpmLock(npm));
  return root;
}
/** Offline registry double: a mutable state object so a test can move `latest` or yank between calls. */
function registry(state) {
  const calls = [];
  const json = (body, status = 200) => new Response(typeof body === "string" ? body : JSON.stringify(body), { status });
  const fetchImpl = async (url) => {
    calls.push(String(url));
    if (state.down) throw new Error("ECONNREFUSED");
    const u = new URL(url);
    if (u.host === "index.crates.io") {
      const name = u.pathname.split("/").at(-1);
      const rows = state.cargo?.[name];
      return rows ? json(rows.map((row) => JSON.stringify(row)).join("\n")) : json("", 404);
    }
    if (u.host === "crates.io") {
      if (state.apiDown) return json("", 503);
      const name = u.pathname.split("/").at(-1);
      return state.latest?.cargo?.[name] ? json({ crate: { max_stable_version: state.latest.cargo[name] } }) : json("", 404);
    }
    if (u.host === "registry.npmjs.org") {
      const doc = state.npm?.[decodeURIComponent(u.pathname.slice(1))];
      return doc ? json(doc) : json("", 404);
    }
    return json("", 404);
  };
  return { fetchImpl, calls };
}
const cargoRow = (version, cksum, extra = {}) => ({ name: "rightkit-foo", vers: version, cksum, yanked: false, ...extra });
const npmDoc = (versions, latest, deprecated) => ({ "dist-tags": { latest }, versions: Object.fromEntries(Object.entries(versions).map(([v, integrity]) => [v, { dist: { integrity }, ...(deprecated?.[v] ? { deprecated: deprecated[v] } : {}) }])) });
const opts = (root, reg, extra = {}) => ({ root, versions: bundled, ownership, fetchImpl: reg.fetchImpl, sleep: NO_SLEEP, cacheDir: null, attempts: 2, now: () => Date.parse("2026-10-23T12:00:00Z"), warn() {}, ...extra });
const rejects = (promise, pattern, code) => assert.rejects(promise, (error) => {
  assert.ok(error instanceof RightKitRegistryError, String(error));
  assert.match(error.message, pattern);
  if (code) assert.equal(error.code, code);
  return true;
});

/** The controller's side: fresh admission freezes the snapshot and records it for this release in its state root. */
const IDENT = { releaseId: "app-1.0.0-abcd1234", commit: "abcd1234".repeat(5), platform: "win" };
async function admit(t, root, reg, extra = {}) {
  const stateRoot = path.join(tempDir(t), "state");
  mkdirSync(stateRoot, { recursive: true });
  const resolution = await prepareRightKitResolution({ ...opts(root, reg, extra), snapshotFile: path.join(stateRoot, SNAPSHOT_FILE), write: true, admission: true });
  writeFileSync(path.join(stateRoot, "release-inputs.lock.json"), JSON.stringify({ schema: 1, ...IDENT, createdAt: Date.now() }));
  writeAdmissionRecord(stateRoot, { ...IDENT, snapshot: resolution.snapshot });
  return { stateRoot, resolution, file: path.join(stateRoot, SNAPSHOT_FILE) };
}
const worker = (root, reg, boundSnapshot, extra = {}) => prepareRightKitResolution({ ...opts(root, reg, extra), boundSnapshot, reuse: true, admission: true });
const lockedFoo = (version = "0.2.0", checksum = CK("2")) => ({ cargo: [{ name: "rightkit-foo", version, checksum }] });
const fooRegistry = (extra = {}) => registry({ cargo: { "rightkit-foo": [cargoRow("0.2.0", CK("2"))] }, latest: { cargo: { "rightkit-foo": "0.2.0" } }, ...extra });

test("a newly published exact version absent from the bundled file resolves with an unchanged resolver package", async (t) => {
  const root = app(t, { cargo: [{ name: "rightkit-foo", version: "0.9.0", checksum: CK("a") }], npm: [{ name: "@rightkit/bar", version: "0.7.0", integrity: INTEGRITY("b") }] });
  const reg = registry({ cargo: { "rightkit-foo": [cargoRow("0.1.0", CK("1")), cargoRow("0.9.0", CK("a"))] }, npm: { "@rightkit/bar": npmDoc({ "0.4.1": INTEGRITY("1"), "0.7.0": INTEGRITY("b") }, "0.7.0") }, latest: { cargo: { "rightkit-foo": "0.9.0" } } });
  const snapshot = await resolveRightKitPins(opts(root, reg));
  assert.deepEqual(snapshot.entries.map((e) => `${e.ecosystem}:${e.name}@${e.version}`), ["cargo:rightkit-foo@0.9.0", "npm:@rightkit/bar@0.7.0"]);
  assert.equal(snapshot.entries[0].registryDigest, CK("a"));
  assert.equal(snapshot.registries.cargo, "https://index.crates.io/");
  assert.match(snapshot.observedAt, /^2026-10-23T12:00:00/);
  // The bundled-only checks refuse both versions; the verified maps make them pass, exactly and only them.
  assert.equal(rightKitVersionAllowed(new Set(["0.4.1"]), "0.7.0"), false);
  const maps = verifiedVersionMaps(snapshot);
  assert.equal(maps.npm.get("@rightkit/bar").has("0.7.0"), true);
  const merged = new Map([["rightkit-foo", new Set(["0.1.0", ...maps.cargo.get("rightkit-foo")])]]);
  assert.equal(assertPublishedRightKitCargoDependencies([{ name: "rightkit-foo", source: SOURCE, req: "=0.9.0" }], merged, "app"), 1);
  assert.throws(() => assertPublishedRightKitCargoDependencies([{ name: "rightkit-foo", source: SOURCE, req: "=0.9.0" }], new Map([["rightkit-foo", new Set(["0.1.0"])]]), "app"), /exact crates.io versions/);
});

test("npm and Cargo are symmetric: both allow-lists carry the registry maps and both ecosystems are verified the same way", async (t) => {
  const source = readFileSync(new URL("./release.mjs", import.meta.url), "utf8");
  assert.match(source, /RIGHTKIT_CARGO_ALLOWED = buildAllowedVersions\(\s*RIGHTKIT_VERSIONS\.cargo,\s*RIGHTKIT_VERSIONS\.stagedCargo,\s*RIGHTKIT_VERSIONS\.legacyCargo,\s*RIGHTKIT_VERSIONS\.registry\?\.cargo,\s*\)/);
  assert.match(source, /RIGHTKIT_ALLOWED = buildAllowedVersions\(\s*RIGHTKIT_VERSIONS\.npm,[\s\S]*RIGHTKIT_VERSIONS\.registry\?\.npm,\s*\)/);
  for (const [label, mutate, pattern] of [
    ["yanked/deprecated", (s) => { s.cargo["rightkit-foo"][0].yanked = true; s.npm["@rightkit/bar"] = npmDoc({ "0.4.1": INTEGRITY("1") }, "0.4.1", { "0.4.1": "do not use" }); }, /yanked on crates\.io[\s\S]*deprecated on npm/],
    ["unpublished", (s) => { s.cargo["rightkit-foo"] = [cargoRow("0.0.5", CK("9"))]; s.npm["@rightkit/bar"] = npmDoc({ "0.0.1": INTEGRITY("9") }, "0.0.1"); }, /not published on crates\.io[\s\S]*not published on npm/],
    ["digest", (s) => { s.cargo["rightkit-foo"][0].cksum = CK("f"); s.npm["@rightkit/bar"].versions["0.4.1"].dist.integrity = INTEGRITY("f"); }, /checksum mismatch[\s\S]*integrity mismatch/],
  ]) {
    const root = app(t, { cargo: [{ name: "rightkit-foo", version: "0.1.0", checksum: CK("1") }], npm: [{ name: "@rightkit/bar", version: "0.4.1", integrity: INTEGRITY("1") }] });
    const state = { cargo: { "rightkit-foo": [cargoRow("0.1.0", CK("1"))] }, npm: { "@rightkit/bar": npmDoc({ "0.4.1": INTEGRITY("1") }, "0.4.1") }, latest: { cargo: { "rightkit-foo": "0.1.0" } } };
    mutate(state);
    await rejects(resolveRightKitPins(opts(root, registry(state))), pattern, "refused");
    assert.ok(label);
  }
});

test("a publication between admission and seal leaves the frozen snapshot unchanged and is never re-checked", async (t) => {
  const root = app(t, lockedFoo());
  const state = { cargo: { "rightkit-foo": [cargoRow("0.2.0", CK("2"))] }, latest: { cargo: { "rightkit-foo": "0.2.0" } } };
  const reg = registry(state);
  const admitted = await admit(t, root, reg);
  assert.equal(admitted.resolution.source, "resolved");
  const before = readFileSync(admitted.file, "utf8");
  state.latest.cargo["rightkit-foo"] = "0.5.0"; // published after admission: the pin is now behind latest
  state.cargo["rightkit-foo"].push(cargoRow("0.5.0", CK("5")));
  const calls = reg.calls.length;
  const bound = loadAdmittedSnapshot(admitted.stateRoot, IDENT);
  const sealed = await worker(root, reg, bound);
  assert.equal(sealed.source, "bound");
  assert.deepEqual(sealed.freshness.refusals, []);
  assert.deepEqual(sealed.freshness.newer, []);
  assert.deepEqual(sealed.snapshot.entries.map((e) => e.version), ["0.2.0"]);
  assert.equal(sealed.snapshot.latest[0].latest, "0.2.0");
  assert.equal(reg.calls.length, calls, "seal or resume re-resolved");
  assert.equal(readFileSync(admitted.file, "utf8"), before);
  // A new admission (a different build) sees the publication, verifies exactly the locked 0.2.0 and never rewrites the pin.
  const fresh = await resolveRightKitPins(opts(root, reg));
  assert.equal(fresh.latest[0].latest, "0.5.0");
  assert.deepEqual(fresh.entries.map((e) => e.version), ["0.2.0"]);
  await rejects(prepareRightKitResolution({ ...opts(root, reg), admission: true }), /rightkit-foo pinned 0\.2\.0 -> latest 0\.5\.0/, "stale-pin");
});

// ----------------------------------------------------------------------------------------------- finding 1: held rows

test("admission refuses a stale pin even when .rightkit/update.json holds it; the hold is reported as a lane task, not an exception", async (t) => {
  const root = app(t, { cargo: [{ name: "rightkit-foo", version: "0.2.0", checksum: CK("2") }], npm: [{ name: "@rightkit/bar", version: "0.4.1", integrity: INTEGRITY("1") }] });
  writeFileSync(path.join(root, ".git"), "");
  mkdirSync(path.join(root, ".rightkit"));
  const write = (policy) => writeFileSync(path.join(root, ".rightkit", "update.json"), JSON.stringify({ schemaVersion: 1, sources: [], policy: { default: "auto-land", ...policy } }));
  const reg = registry({
    cargo: { "rightkit-foo": [cargoRow("0.2.0", CK("2"))] }, npm: { "@rightkit/bar": npmDoc({ "0.4.1": INTEGRITY("1"), "0.5.0": INTEGRITY("5") }, "0.5.0") },
    latest: { cargo: { "rightkit-foo": "0.2.0" } },
  });
  write({ held: ["@rightkit/bar"] });
  await assert.rejects(prepareRightKitResolution({ ...opts(root, reg), admission: true }), (error) => error.code === "stale-pin"
    && /@rightkit\/bar pinned 0\.4\.1 -> latest 0\.5\.0.*\[held by \.rightkit\/update\.json \(policy\.held\): a hold is a lane task, not permission to seal stale\]/.test(error.message));
  write({ tier: { held: ["/^@rightkit\\/ba/"] } });
  await rejects(prepareRightKitResolution({ ...opts(root, reg), admission: true }), /tier\.held/, "stale-pin");
  // Doctor mode: informational, same wording, plus the lane task.
  write({ held: ["@rightkit/bar"] });
  const doctor = await prepareRightKitResolution(opts(root, reg));
  assert.deepEqual(doctor.freshness.refusals, []);
  assert.match(doctor.freshness.newer[0], /@rightkit\/bar pinned 0\.4\.1 -> latest 0\.5\.0.*right-update run/);
  assert.match(doctor.freshness.tasks[0], /migration lane task, not current and not permission to seal/);
  // Unheld and equal-to-latest pins are not tasks.
  write({});
  const equal = await prepareRightKitResolution({ ...opts(root, registry({ cargo: { "rightkit-foo": [cargoRow("0.2.0", CK("2"))] }, npm: { "@rightkit/bar": npmDoc({ "0.4.1": INTEGRITY("1") }, "0.4.1") }, latest: { cargo: { "rightkit-foo": "0.2.0" } } })), admission: true });
  assert.deepEqual(equal.freshness, { refusals: [], newer: [], tasks: [], warnings: [], notes: [] });
});

test("the built-in right-update held pattern (sign|secur|licen|credential) is not read: it neither holds nor excuses a pin", async (t) => {
  const own = { ...ownership, capabilities: [...ownership.capabilities, { package: "rightkit-license" }] };
  const root = app(t, { cargo: [{ name: "rightkit-license", version: "0.1.2", checksum: CK("3") }] });
  writeFileSync(path.join(root, ".git"), "");
  mkdirSync(path.join(root, ".rightkit"));
  writeFileSync(path.join(root, ".rightkit", "update.json"), JSON.stringify({ schemaVersion: 1, sources: [], policy: { default: "auto-land" } }));
  const reg = registry({ cargo: { "rightkit-license": [{ name: "rightkit-license", vers: "0.1.2", cksum: CK("3"), yanked: false }, { name: "rightkit-license", vers: "0.2.0", cksum: CK("4"), yanked: false }] }, latest: { cargo: { "rightkit-license": "0.2.0" } } });
  const result = await prepareRightKitResolution({ ...opts(root, reg, { ownership: own }), versions: { ...bundled, cargo: { ...bundled.cargo, "rightkit-license": "0.1.2" } } });
  assert.deepEqual(result.freshness.tasks, []);
  assert.equal(result.freshness.newer.length, 1);
  assert.doesNotMatch(result.freshness.newer[0], /held/);
});

// ----------------------------------------------------------------------------------------------- finding 2: lock coverage

test("a declared RightKit dependency with no lock entry is refused: no bundled-only fallback; no-dependency and non-registry cases stay explicit", async (t) => {
  const reg = registry({});
  // npm: package.json declares @rightkit/bar at a bundled-known version, no pnpm-lock.yaml at all.
  let root = tempDir(t);
  writeFileSync(path.join(root, "package.json"), JSON.stringify({ devDependencies: { "@rightkit/bar": "0.4.1" } }));
  await rejects(resolveRightKitPins(opts(root, reg)), /package\.json declares @rightkit\/bar but no pnpm-lock\.yaml entry proves it/);
  // cargo: Cargo.toml declares rightkit-foo (also through a rename and a target table), no Cargo.lock.
  root = tempDir(t);
  writeFileSync(path.join(root, "Cargo.toml"), '[package]\nname = "x"\nversion = "0.0.0"\n[dependencies]\nfoo2 = { package = "rightkit-foo", version = "=0.1.0" }\n');
  await rejects(resolveRightKitPins(opts(root, reg)), /Cargo\.toml declares rightkit-foo but no Cargo\.lock entry proves it/);
  // an incomplete lock (other packages only) is the same refusal.
  writeFileSync(path.join(root, "Cargo.lock"), cargoLock([{ name: "serde", version: "1.0.0", checksum: CK("1") }]));
  await rejects(resolveRightKitPins(opts(root, reg)), /declares rightkit-foo but no Cargo\.lock entry proves it/);
  // explicit no-dependency app: admitted with zero entries and no network.
  root = tempDir(t);
  writeFileSync(path.join(root, "package.json"), JSON.stringify({ dependencies: { react: "19.0.0" } }));
  writeFileSync(path.join(root, "Cargo.toml"), '[package]\nname = "x"\nversion = "0.0.0"\n[dependencies]\nserde = "1"\n');
  const none = await prepareRightKitResolution({ ...opts(root, reg), admission: true });
  assert.equal(none.snapshot.entries.length, 0);
  assert.equal(reg.calls.length, 0);
  // a git/link specifier is refused by the package-contract validator by name, not counted as a missing lock entry.
  root = tempDir(t);
  writeFileSync(path.join(root, "package.json"), JSON.stringify({ dependencies: { "@rightkit/bar": "link:../bar" } }));
  assert.equal((await resolveRightKitPins(opts(root, reg))).entries.length, 0);
});

test("a lock removed after admission refuses the worker instead of falling back to the bundled data", async (t) => {
  const root = app(t, lockedFoo());
  writeFileSync(path.join(root, "Cargo.toml"), '[package]\nname = "x"\nversion = "0.0.0"\n[dependencies]\nrightkit-foo = "=0.2.0"\n');
  const reg = fooRegistry();
  const admitted = await admit(t, root, reg);
  const bound = loadAdmittedSnapshot(admitted.stateRoot, IDENT);
  writeFileSync(path.join(root, "Cargo.lock"), cargoLock([]));
  await rejects(worker(root, reg, bound), /declares rightkit-foo but no Cargo\.lock entry proves it/, "snapshot-mismatch");
});

// ----------------------------------------------------------------------------------------------- finding 3: resume without a snapshot

test("a resume never resolves again over missing frozen evidence", async (t) => {
  const root = app(t, lockedFoo());
  const reg = fooRegistry();
  await rejects(prepareRightKitResolution({ ...opts(root, reg), reuse: true, admission: false }), /admitted RightKit registry snapshot is missing/, "snapshot-missing");
  assert.equal(reg.calls.length, 0);
  const stateRoot = path.join(tempDir(t), "state");
  mkdirSync(stateRoot, { recursive: true });
  await assert.rejects(async () => loadAdmittedSnapshot(stateRoot, IDENT), (error) => error.code === "no-admission" && /no admission record exists/.test(error.message));
  const admitted = await admit(t, root, reg);
  rmSync(admitted.file);
  await assert.rejects(async () => loadAdmittedSnapshot(admitted.stateRoot, { ...IDENT, inPreflight: true }), (error) => error.code === "no-admission" && /snapshot file is missing/.test(error.message));
});

// ----------------------------------------------------------------------------------------------- finding 4: evidence

test("sealed manifests require versioned registry evidence; only a named pre-0.6.0 release is exempt", async (t) => {
  const root = app(t, lockedFoo());
  const { resolution } = await admit(t, root, fooRegistry());
  const fields = registryManifestFields(resolution.snapshot, "0.6.0");
  assert.equal(fields.rightkitRegistryEvidence, 1);
  assert.equal(assertManifestRegistryEvidence({ ...fields }).legacy, false);
  assert.throws(() => assertManifestRegistryEvidence({ releaseId: "r" }, { legacyReleaseIds: [] }), /no RightKit registry evidence; refusing/);
  assert.deepEqual(assertManifestRegistryEvidence({ releaseId: "r" }, { legacyReleaseIds: ["r"] }), { legacy: true });
  assert.throws(() => assertManifestRegistryEvidence({ releaseId: "r", rightReleaseVersion: "0.6.0" }, { legacyReleaseIds: ["r"] }), /stripped new seal is never a legacy seal/);
  assert.throws(() => assertManifestRegistryEvidence({ rightkitRegistry: fields.rightkitRegistry }, { legacyReleaseIds: [] }), /marker must be 1, got <missing>/);
  assert.throws(() => assertManifestRegistryEvidence({ rightkitRegistryEvidence: 1 }, { legacyReleaseIds: [] }), /evidence is missing/);
  assert.throws(() => assertManifestRegistryEvidence({ rightkitRegistryEvidence: 2, rightkitRegistry: fields.rightkitRegistry }), /marker must be 1/);
  assert.throws(() => assertManifestRegistryEvidence({ ...fields, rightkitRegistry: { ...fields.rightkitRegistry, snapshot: { ...resolution.snapshot, observedAt: "2020-01-01T00:00:00Z" } } }), /digest/);
  // a doctor-mode (inspection) snapshot is not admission evidence.
  const inspection = await resolveRightKitPins(opts(root, fooRegistry()));
  assert.throws(() => assertManifestRegistryEvidence(registryManifestFields(inspection, "0.6.0")), /not taken at build admission/);
});

// ----------------------------------------------------------------------------------------------- finding 5: latest observations

test("the latest row is always the live registry state; a withdrawn previous latest needs no cache deletion and a stale answer is a recoverable conflict", async (t) => {
  const cacheDir = path.join(tempDir(t), "cache");
  const root = app(t, lockedFoo());
  const state = { cargo: { "rightkit-foo": [cargoRow("0.2.0", CK("2")), cargoRow("0.4.0", CK("4"))] }, latest: { cargo: { "rightkit-foo": "0.4.0" } } };
  const cacheFile = path.join(cacheDir, "latest", "cargo", "rightkit-foo.json");
  let snapshot = await resolveRightKitPins(opts(root, registry(state), { cacheDir }));
  assert.equal(snapshot.latest[0].latest, "0.4.0");
  assert.equal(JSON.parse(readFileSync(cacheFile, "utf8")).highWater.version, "0.4.0");
  // The registry answers lower while 0.4.0 is still published: stale or out of order. Never downgrade silently.
  state.latest.cargo["rightkit-foo"] = "0.3.0";
  state.cargo["rightkit-foo"].push(cargoRow("0.3.0", CK("3")));
  snapshot = await resolveRightKitPins(opts(root, registry(state), { cacheDir }));
  assert.equal(snapshot.latest[0].latest, "0.3.0", "reported as the live answer, not max(previous, live)");
  assert.equal(snapshot.latest[0].via, "live");
  assert.deepEqual(snapshot.latest[0].conflict, { highWater: "0.4.0" });
  assert.equal(JSON.parse(readFileSync(cacheFile, "utf8")).highWater.version, "0.4.0");
  await assert.rejects(resolveRightKitPins(opts(root, registry(state), { cacheDir, requireLiveLatest: true })), (error) => error.recoverable === true && error.code === "eligibility-conflict" && /0\.4\.0 .*is still published/.test(error.message) && /no cache deletion is needed/.test(error.message));
  // 0.4.0 is yanked: a withdrawal. The live state wins on its own and the mark follows it; nothing is deleted by hand.
  state.cargo["rightkit-foo"].find((row) => row.vers === "0.4.0").yanked = true;
  snapshot = await resolveRightKitPins(opts(root, registry(state), { cacheDir, requireLiveLatest: true }));
  assert.equal(snapshot.latest[0].latest, "0.3.0");
  assert.deepEqual(snapshot.latest[0].withdrawn.version, "0.4.0");
  assert.equal(JSON.parse(readFileSync(cacheFile, "utf8")).highWater.version, "0.3.0");
  assert.match(evaluateRightKitFreshness(snapshot, null).notes.join("\n"), /previously observed latest 0\.4\.0 was withdrawn; the live registry state \(0\.3\.0\) is used/);
});

test("freshness outage at admission is a recoverable failure even with a cached latest; outside admission the cache is used with its age", async (t) => {
  const cacheDir = path.join(tempDir(t), "cache");
  const root = app(t, lockedFoo());
  const state = { cargo: { "rightkit-foo": [cargoRow("0.2.0", CK("2"))] }, latest: { cargo: { "rightkit-foo": "0.2.0" } }, apiDown: true };
  await assert.rejects(resolveRightKitPins(opts(root, registry(state), { cacheDir })), (error) => error.recoverable === true && /no cached latest observation/.test(error.message));
  await assert.rejects(prepareRightKitResolution({ ...opts(root, registry(state), { cacheDir }), admission: true }), (error) => error.recoverable === true && error.code === "registry-outage" && /freshness cannot be established at admission/.test(error.message));
  state.apiDown = false;
  await resolveRightKitPins(opts(root, registry(state), { cacheDir }));
  state.apiDown = true;
  const later = Date.parse("2026-10-23T12:00:00Z") + 5 * 3600 * 1000;
  await assert.rejects(prepareRightKitResolution({ ...opts(root, registry(state), { cacheDir, now: () => later }), admission: true }), (error) => error.recoverable === true && /last-observed, not current/.test(error.message));
  const snapshot = await resolveRightKitPins(opts(root, registry(state), { cacheDir, now: () => later }));
  assert.equal(snapshot.latest[0].via, "cache");
  assert.equal(snapshot.latest[0].ageMs, 5 * 3600 * 1000);
  assert.match(evaluateRightKitFreshness(snapshot, null).notes[0], /taken from cache, 5 hours old/);
  const verdict = evaluateRightKitFreshness({ ...snapshot, latest: [{ ...snapshot.latest[0], latest: "0.9.0" }] }, null);
  assert.match(verdict.newer[0], /pinned 0\.2\.0 -> latest 0\.9\.0 \(latest from cached observation .* 5 hours old; last-observed, not current\)/);
});

// ----------------------------------------------------------------------------------------------- ruling 4: no offline override

test("there is no offline override: a registry outage fails fresh admission even with a primed cache and the old environment switch", async (t) => {
  const cacheDir = path.join(tempDir(t), "cache");
  const root = app(t, lockedFoo());
  const state = { cargo: { "rightkit-foo": [cargoRow("0.2.0", CK("2"))] }, latest: { cargo: { "rightkit-foo": "0.2.0" } } };
  await resolveRightKitPins(opts(root, registry(state), { cacheDir }));
  state.down = true;
  process.env.RIGHT_RELEASE_RIGHTKIT_OFFLINE = "1";
  t.after(() => { delete process.env.RIGHT_RELEASE_RIGHTKIT_OFFLINE; });
  await assert.rejects(prepareRightKitResolution({ ...opts(root, registry(state), { cacheDir }), admission: true }), (error) => error.recoverable === true && error.code === "registry-outage" && /no offline override for a fresh admission/.test(error.message));
});

test("outage retries are bounded", async (t) => {
  const root = app(t, lockedFoo());
  const reg = registry({ down: true });
  await rejects(resolveRightKitPins(opts(root, reg, { attempts: 3 })), /unreachable after 3 attempts/);
  assert.equal(reg.calls.filter((url) => url.includes("index.crates.io")).length, 3);
});

// ----------------------------------------------------------------------------------------------- finding 6: provenance

test("a worker accepts a snapshot only through this release's admitted state", async (t) => {
  const root = app(t, lockedFoo());
  const reg = fooRegistry();
  const admitted = await admit(t, root, reg);
  const good = loadAdmittedSnapshot(admitted.stateRoot, IDENT);
  assert.equal((await worker(root, reg, good)).source, "bound");

  // arbitrary snapshot path (not the state root's own snapshot) / missing state.
  const elsewhere = path.join(tempDir(t), "snap.json");
  writeFileSync(elsewhere, readFileSync(admitted.file));
  assert.throws(() => loadAdmittedSnapshot(admitted.stateRoot, { ...IDENT, snapshotFile: elsewhere }), (e) => e.code === "no-admission" && /not the admitted snapshot of this release/.test(e.message));

  // a foreign release's snapshot: other release id, other commit, other platform.
  for (const foreign of [{ releaseId: "other-1.0.0-abcd1234" }, { commit: "f".repeat(40) }, { platform: "mac" }]) {
    assert.throws(() => loadAdmittedSnapshot(admitted.stateRoot, { ...IDENT, ...foreign }), (e) => e.code === "no-admission" && /belongs to another release/.test(e.message));
  }
  // the same snapshot copied into another release's state root has no admission record there.
  const other = path.join(tempDir(t), "other");
  mkdirSync(other, { recursive: true });
  writeFileSync(path.join(other, SNAPSHOT_FILE), readFileSync(admitted.file));
  writeFileSync(path.join(other, "release-inputs.lock.json"), JSON.stringify({ schema: 1, ...IDENT }));
  assert.throws(() => loadAdmittedSnapshot(other, IDENT), (e) => e.code === "no-admission" && /no admission record/.test(e.message));

  // the admitted snapshot replaced after admission (digest differs from the record).
  const replaced = { ...JSON.parse(readFileSync(admitted.file, "utf8")), observedAt: "2020-01-01T00:00:00.000Z" };
  replaced.digest = snapshotDigest(replaced);
  writeFileSync(admitted.file, JSON.stringify(replaced));
  assert.throws(() => loadAdmittedSnapshot(admitted.stateRoot, IDENT), (e) => /not the one admitted for this release/.test(e.message));
});

test("a fabricated self-hashed snapshot is refused with or without an admission record", async (t) => {
  const root = app(t, lockedFoo());
  const reg = fooRegistry();
  const forge = (mutate) => {
    const snapshot = { schema: 1, mode: "admission", observedAt: "2026-10-23T00:00:00.000Z", registries: {}, policyRevision: "x", lockFiles: [], entries: [{ ecosystem: "cargo", name: "rightkit-foo", version: "0.2.0", lockDigest: CK("2"), registryDigest: CK("2"), lockFiles: ["Cargo.lock"], verifiedAt: "x", via: "live" }], latest: [{ ecosystem: "cargo", name: "rightkit-foo", latest: "0.2.0", observedAt: "x", via: "live" }] };
    mutate?.(snapshot);
    snapshot.digest = snapshotDigest(snapshot);
    return snapshot;
  };
  const stateWith = (snapshot, { record = true } = {}) => {
    const stateRoot = path.join(tempDir(t), "state");
    mkdirSync(stateRoot, { recursive: true });
    writeFileSync(path.join(stateRoot, SNAPSHOT_FILE), JSON.stringify(snapshot));
    writeFileSync(path.join(stateRoot, "release-inputs.lock.json"), JSON.stringify({ schema: 1, ...IDENT }));
    if (record) writeAdmissionRecord(stateRoot, { ...IDENT, snapshot });
    return stateRoot;
  };
  // 1. well-formed forgery, nobody recorded its admission.
  assert.throws(() => loadAdmittedSnapshot(stateWith(forge(), { record: false }), IDENT), (e) => e.code === "no-admission");
  // 2. forged and recorded, but internally inconsistent: arbitrary registry digest; missing latest; extra verified entry; non-admission mode.
  const cases = [
    ["arbitrary registry digest", (s) => { s.entries[0].registryDigest = CK("9"); }, /registry digest does not equal the lock digest/],
    ["latest missing", (s) => { s.latest = []; }, /no latest observation for rightkit-foo/],
    ["latest for an unverified package", (s) => { s.latest.push({ ecosystem: "npm", name: "@rightkit/bar", latest: "9.9.9", observedAt: "x", via: "live" }); }, /has no verified entry/],
    ["not live", (s) => { s.entries[0].via = "offline-snapshot"; }, /not verified live/],
    ["non-admission mode", (s) => { s.mode = "inspection"; }, /not taken at build admission/],
    ["cached latest", (s) => { s.latest[0].via = "cache"; }, /non-live latest observations/],
  ];
  for (const [label, mutate, pattern] of cases) {
    await assert.rejects(async () => loadAdmittedSnapshot(stateWith(forge(mutate)), IDENT), (e) => e instanceof RightKitRegistryError && pattern.test(e.message), label);
  }
  // 3. consistent with an unrelated verified entry the locks do not contain: refused against the current locks.
  const extra = forge((s) => {
    s.entries.push({ ecosystem: "npm", name: "@rightkit/bar", version: "0.4.1", lockDigest: INTEGRITY("1"), registryDigest: INTEGRITY("1"), lockFiles: [], verifiedAt: "x", via: "live" });
    s.latest.push({ ecosystem: "npm", name: "@rightkit/bar", latest: "0.4.1", observedAt: "x", via: "live" });
  });
  const bound = loadAdmittedSnapshot(stateWith(extra), IDENT);
  await rejects(worker(root, reg, bound), /@rightkit\/bar 0\.4\.1 is in the bound registry snapshot but not in the locked inputs/, "snapshot-mismatch");
  // 4. a consistent forgery whose lock digest differs from the real lock.
  const wrongLock = forge((s) => { s.entries[0].lockDigest = CK("7"); s.entries[0].registryDigest = CK("7"); });
  await rejects(worker(root, reg, loadAdmittedSnapshot(stateWith(wrongLock), IDENT)), /lock digest changed since the bound registry snapshot/, "snapshot-mismatch");
});

test("changed locks or a changed release input identity invalidate the admitted state", async (t) => {
  const root = app(t, lockedFoo());
  const reg = fooRegistry();
  const admitted = await admit(t, root, reg);
  const bound = loadAdmittedSnapshot(admitted.stateRoot, IDENT);
  writeFileSync(path.join(root, "Cargo.lock"), cargoLock(lockedFoo("0.3.0", CK("3")).cargo));
  const calls = reg.calls.length;
  await rejects(worker(root, reg, bound), /rightkit-foo 0\.3\.0 is locked but not in the bound registry snapshot[\s\S]*rightkit-foo 0\.2\.0 is in the bound registry snapshot but not in the locked inputs/, "snapshot-mismatch");
  writeFileSync(path.join(root, "Cargo.lock"), cargoLock(lockedFoo("0.2.0", CK("8")).cargo));
  await rejects(worker(root, reg, bound), /lock digest changed since the bound registry snapshot/, "snapshot-mismatch");
  assert.equal(reg.calls.length, calls, "mismatch must never trigger a new resolution");
  // the controller's lock payload (release identity/input identity) changed after admission.
  writeFileSync(path.join(admitted.stateRoot, "release-inputs.lock.json"), JSON.stringify({ schema: 1, ...IDENT, tampered: true }));
  assert.throws(() => loadAdmittedSnapshot(admitted.stateRoot, IDENT), (e) => /release input identity changed since admission/.test(e.message));
  // resume: preflight accepts the admitted snapshot before the controller rewrites the payload and the record.
  assert.equal(loadAdmittedSnapshot(admitted.stateRoot, { ...IDENT, inPreflight: true }).digest, bound.digest);
});

// ----------------------------------------------------------------------------------------------- caller / handoff

test("build-release hands the admitted snapshot to the worker through the state root: admission, reuse path and record are wired", () => {
  const source = readFileSync(new URL("./build-release.mjs", import.meta.url), "utf8");
  // fresh build: live admission, snapshot written into the release state root.
  assert.match(source, /const rightKitSnapshotFile = path\.join\(stateRoot, SNAPSHOT_FILE\);/);
  assert.match(source, /admission: !rightKitReuse,\s*write: true,/);
  // resume and re-run over a sealed release: frozen evidence only, never a fresh resolve.
  assert.match(source, /const rightKitReuse = Boolean\(resumeFrom\) \|\| sealedExists;/);
  assert.match(source, /boundSnapshot = loadAdmittedSnapshot\(stateRoot, \{ \.\.\.admissionIdentity, inPreflight: true \}\);/);
  assert.match(source, /legacySealed = assertManifestRegistryEvidence\(sealedManifest\)\.legacy;/);
  assert.match(source, /reuse: rightKitReuse,/);
  // the worker gets the path, the controller records the admission after the lock payload, the seal carries the fields.
  assert.match(source, /env\[SNAPSHOT_ENV\] = rightKitSnapshotFile;/);
  assert.match(source, /writeJson\(path\.join\(stateRoot, "release-inputs\.lock\.json"\), lockPayload\);\s*[^\n]*\n\s*if \(rightKitResolution\) writeAdmissionRecord\(stateRoot, \{ \.\.\.admissionIdentity, snapshot: rightKitResolution\.snapshot \}\);/);
  assert.match(source, /rightkitRegistryFields: rightKitResolution \? registryManifestFields\(rightKitResolution\.snapshot, RELEASE_TOOL_VERSION\) : undefined/);
  // lock inputs come from the canonical inventory, not a reconstructed list.
  assert.match(source, /extraLocks: rightKitLockInventory\(\{ requiredInputs, cargoLockPaths: pinnedCargoLocks, nativeLayout, config, root: appRoot \}\)/);
  const worker = readFileSync(new URL("./release.mjs", import.meta.url), "utf8");
  assert.match(worker, /loadAdmittedSnapshot\(stateRoot, \{ releaseId: `\$\{config\.app\}-\$\{config\.version\}-\$\{commit\.slice\(0, 8\)\}`, commit, platform: opts\.platform, snapshotFile: rightKitSnapshotFile \}\)/);
  assert.match(worker, /resolveReleaseBuildInputs\(\{ repoRoot, appRoot: root, configPath, config, target: canonicalTarget \}\)/);
});

test("a resume replays the controller's steps: preflight loads the admitted snapshot, then the record is refreshed for the new lock payload", async (t) => {
  const root = app(t, lockedFoo());
  const reg = fooRegistry();
  const admitted = await admit(t, root, reg);
  const calls = reg.calls.length;
  const bound = loadAdmittedSnapshot(admitted.stateRoot, { ...IDENT, inPreflight: true });
  const resumed = await prepareRightKitResolution({ ...opts(root, reg), boundSnapshot: bound, reuse: true, admission: false });
  assert.equal(resumed.source, "bound");
  // preflight rewrites release-inputs.lock.json (new createdAt) and refreshes the record for the same snapshot.
  writeFileSync(path.join(admitted.stateRoot, "release-inputs.lock.json"), JSON.stringify({ schema: 1, ...IDENT, createdAt: Date.now() + 1 }));
  writeAdmissionRecord(admitted.stateRoot, { ...IDENT, snapshot: resumed.snapshot });
  assert.equal((await worker(root, reg, loadAdmittedSnapshot(admitted.stateRoot, IDENT))).source, "bound");
  assert.equal(reg.calls.length, calls, "no registry call on resume or in the worker");
});

// ----------------------------------------------------------------------------------------------- finding 7: native-iOS sibling locks

test("the canonical lock inventory brings a sibling-directory Rust lock under admission (native iOS, nativeLayout null)", async (t) => {
  const repo = tempDir(t);
  const appRoot = path.join(repo, "apps", "coderight");
  const engine = path.join(repo, "engine");
  mkdirSync(appRoot, { recursive: true });
  mkdirSync(engine, { recursive: true });
  writeFileSync(path.join(engine, "Cargo.toml"), '[package]\nname = "engine"\nversion = "0.0.0"\n[dependencies]\nrightkit-foo = "=0.2.0"\n');
  writeFileSync(path.join(engine, "Cargo.lock"), cargoLock([{ name: "rightkit-foo", version: "0.2.0", checksum: CK("2") }]));
  const reg = fooRegistry();
  const inventory = rightKitLockInventory({ requiredInputs: [path.join(appRoot, "right-release.config.mjs"), path.join(engine, "Cargo.toml"), path.join(engine, "Cargo.lock")], cargoLockPaths: [path.join(engine, "Cargo.lock")], nativeLayout: null, config: {}, root: appRoot });
  assert.deepEqual(inventory, [path.join(engine, "Cargo.lock")]);
  // The old reconstructed list (app-root scan only) never saw the sibling lock: it admitted nothing.
  assert.equal((await resolveRightKitPins(opts(appRoot, reg))).entries.length, 0);
  // With the canonical inventory the sibling lock is resolved and verified.
  const snapshot = await resolveRightKitPins(opts(appRoot, reg, { extraLocks: inventory }));
  assert.deepEqual(snapshot.entries.map((e) => `${e.name}@${e.version}`), ["rightkit-foo@0.2.0"]);
  // A stale or unpublished version in that sibling lock is refused, and a sibling manifest without a lock entry too.
  writeFileSync(path.join(engine, "Cargo.lock"), cargoLock([{ name: "rightkit-foo", version: "0.7.0", checksum: CK("7") }]));
  await rejects(resolveRightKitPins(opts(appRoot, reg, { extraLocks: inventory })), /rightkit-foo 0\.7\.0 is not published on crates\.io/);
  writeFileSync(path.join(engine, "Cargo.lock"), cargoLock([]));
  await rejects(resolveRightKitPins(opts(appRoot, reg, { extraLocks: inventory })), /declares rightkit-foo but no Cargo\.lock entry proves it/);
});

// ----------------------------------------------------------------------------------------------- unchanged contracts

test("unknown and prefix-only names, and non-canonical sources, are refused before any network call", async (t) => {
  const reg = registry({});
  let root = app(t, { cargo: [{ name: "rightkit-imposter", version: "1.0.0", checksum: CK("1") }], npm: [{ name: "@rightkit/imposter", version: "1.0.0", integrity: INTEGRITY("1") }] });
  await rejects(resolveRightKitPins(opts(root, reg)), /rightkit-imposter 1\.0\.0[\s\S]*prefix alone does not establish ownership[\s\S]*@rightkit\/imposter/);
  root = app(t, { cargo: [{ name: "rightkit-foo", version: "0.1.0", source: null }, { name: "rightkit-foo", version: "0.1.1", source: "git+https://example.com/x#abc", checksum: CK("1") }, { name: "rightkit-foo", version: "0.1.2", source: SOURCE }], npm: [{ name: "@rightkit/bar", version: "0.4.1", resolution: "tarball: 'https://example.com/b.tgz', integrity: sha512-xx==" }, { name: "@rightkit/bar", version: "0.4.2", resolution: "directory: ../b, type: directory" }] });
  await rejects(resolveRightKitPins(opts(root, reg)), /does not resolve from the canonical crates\.io registry[\s\S]*has no checksum in the lock[\s\S]*does not resolve from the canonical npm registry/);
  assert.equal(reg.calls.length, 0);
});

test("recorded minimum and revocation policy still apply to a registry-proven version", async (t) => {
  const root = app(t, { cargo: [{ name: "rightkit-foo", version: "0.0.9", checksum: CK("1") }] });
  await rejects(resolveRightKitPins(opts(root, registry({}))), /below the recorded minimum 0\.1\.0/);
  const ok = app(t, lockedFoo());
  await rejects(resolveRightKitPins({ ...opts(ok, registry({})), versions: { ...bundled, revoked: { cargo: { "rightkit-foo": ["0.2.0"] } } } }), /0\.2\.0 is revoked by release policy/);
});

test("an app with no RightKit lock entries and no declared RightKit dependencies needs no network", async (t) => {
  const root = app(t, { cargo: [{ name: "serde", version: "1.0.0", checksum: CK("1") }] });
  const reg = registry({});
  const result = await prepareRightKitResolution({ ...opts(root, reg), admission: true });
  assert.equal(result.snapshot.entries.length, 0);
  assert.equal(result.snapshot.mode, "admission");
  assert.equal(reg.calls.length, 0);
  const file = path.join(tempDir(t), "s.json");
  writeFileSync(file, JSON.stringify(result.snapshot));
  assert.equal(loadSnapshot(file).digest, result.snapshot.digest);
});

// ----------------------------------------------------------------------------------------------- documented trust scope

test("DOCUMENTED LIMITATION: a fully consistent forged record that matches the actual locks is accepted; reuse is trusted-local, not authenticated", async (t) => {
  const root = app(t, lockedFoo());
  writeFileSync(path.join(root, "Cargo.toml"), '[package]\nname = "x"\nversion = "0.0.0"\n[dependencies]\nrightkit-foo = "=0.2.0"\n');
  const reg = fooRegistry();
  // A forger who can write the state root never asks the registry: snapshot, admission record and lock payload are all
  // written by hand, consistent with each other and with the real lock (registryDigest = lockDigest, latest = pin, live, admission).
  const forged = { schema: 1, mode: "admission", observedAt: "2026-10-23T00:00:00.000Z", registries: {}, policyRevision: "forged", lockFiles: ["Cargo.lock"],
    entries: [{ ecosystem: "cargo", name: "rightkit-foo", version: "0.2.0", lockDigest: CK("2"), registryDigest: CK("2"), lockFiles: ["Cargo.lock"], verifiedAt: "x", via: "live" }],
    latest: [{ ecosystem: "cargo", name: "rightkit-foo", latest: "0.2.0", observedAt: "x", via: "live" }] };
  forged.digest = snapshotDigest(forged);
  const stateRoot = path.join(tempDir(t), "state");
  mkdirSync(stateRoot, { recursive: true });
  writeFileSync(path.join(stateRoot, SNAPSHOT_FILE), JSON.stringify(forged));
  writeFileSync(path.join(stateRoot, "release-inputs.lock.json"), JSON.stringify({ schema: 1, ...IDENT }));
  writeAdmissionRecord(stateRoot, { ...IDENT, snapshot: forged });
  const calls = reg.calls.length;
  const accepted = await worker(root, reg, loadAdmittedSnapshot(stateRoot, IDENT));
  assert.equal(accepted.source, "bound");
  assert.equal(reg.calls.length, calls, "the registry is not asked again on reuse");
  assert.match(accepted.summary, /bound snapshot \(trusted-local record, not re-checked against the registry, not authenticated\)/);
  // The canonical controller resume accepts the same files too.
  assert.equal(loadAdmittedSnapshot(stateRoot, { ...IDENT, inPreflight: true }).digest, forged.digest);
  // What IS caught is inconsistency with this release or the locks (see the tests above).
});

test("an npm latest tag moved lower on purpose is followed; a derived crates.io answer lower than a published version stays a conflict", async (t) => {
  const cacheDir = path.join(tempDir(t), "cache");
  const root = app(t, { npm: [{ name: "@rightkit/bar", version: "0.4.1", integrity: INTEGRITY("1") }] });
  const state = { npm: { "@rightkit/bar": npmDoc({ "0.4.1": INTEGRITY("1"), "0.5.0": INTEGRITY("5") }, "0.5.0") } };
  await resolveRightKitPins(opts(root, registry(state), { cacheDir }));
  state.npm["@rightkit/bar"]["dist-tags"].latest = "0.4.1"; // maintainer moved the tag back; 0.5.0 stays published
  const snapshot = await resolveRightKitPins(opts(root, registry(state), { cacheDir, requireLiveLatest: true }));
  assert.equal(snapshot.latest[0].latest, "0.4.1");
  assert.deepEqual(snapshot.latest[0].moved.from, "0.5.0");
  assert.equal(snapshot.latest[0].conflict, undefined);
  assert.equal(JSON.parse(readFileSync(path.join(cacheDir, "latest", "npm", "@rightkit%2fbar.json"), "utf8")).highWater.version, "0.4.1");
  assert.match(evaluateRightKitFreshness(snapshot, null).notes.join("\n"), /npm latest tag moved from 0\.5\.0 to 0\.4\.1; the registry is followed/);
});

test("a present but malformed rightReleaseVersion is refused, never treated as legacy", async () => {
  const { assertManifestRegistryEvidence } = await import("./rightkit-registry-resolver.mjs");
  for (const bad of ["", "zero.six", 60, "0.6", null]) {
    assert.throws(() => assertManifestRegistryEvidence({ releaseId: "r-legacy", rightReleaseVersion: bad }, { legacyReleaseIds: ["r-legacy"] }), /rightReleaseVersion is malformed/, String(bad));
  }
  assert.deepEqual(assertManifestRegistryEvidence({ releaseId: "r-legacy" }, { legacyReleaseIds: ["r-legacy"] }), { legacy: true });
  assert.deepEqual(assertManifestRegistryEvidence({ releaseId: "r-legacy", rightReleaseVersion: "0.5.1" }, { legacyReleaseIds: ["r-legacy"] }), { legacy: true });
});
