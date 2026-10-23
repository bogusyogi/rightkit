// Test stand-ins for the controller (build-release.mjs): spawn environment for release.mjs fixtures.
import { execFileSync } from "../background-process.mjs";
import { readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { tempRoot } from "./temp-roots.mjs";
import { collectLockedRightKit, ownedRightKitNames, rightKitLockInventory, registryManifestFields, snapshotDigest, writeAdmissionRecord, writeSnapshot, SNAPSHOT_FILE } from "../rightkit-registry-resolver.mjs";

const dir = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const stub = pathToFileURL(path.join(dir, "test-support", "registry-stub.mjs")).href;
const versions = JSON.parse(readFileSync(path.join(dir, "rightkit-versions.json"), "utf8"));
const ownership = JSON.parse(readFileSync(path.join(dir, "rightkit-ownership.json"), "utf8"));

/** Registry stub for a spawned doctor/worker: the locks under appRoot are the published world. */
export function registryEnv(appRoot, extra = {}) {
  return { NODE_OPTIONS: `${process.env.NODE_OPTIONS ?? ""} --import=${stub}`.trim(), RK_TEST_REGISTRY_ROOT: path.resolve(appRoot), ...extra };
}

/** What the controller leaves in a release state root after admission, for a worker run of this fixture. */
export function admittedWorkerEnv(configPath, platform = "win", { mutate } = {}) {
  const root = path.dirname(configPath);
  const commit = execFileSync("git", ["rev-parse", "HEAD"], { cwd: root, encoding: "utf8" }).trim();
  const config = JSON.parse(execFileSync(process.execPath, ["-e", `import(${JSON.stringify(pathToFileURL(configPath).href)}).then((m) => console.log(JSON.stringify({ app: m.default.app, version: m.default.version })))`], { encoding: "utf8" }));
  const owned = ownedRightKitNames(versions, ownership);
  const repoRoot = execFileSync("git", ["rev-parse", "--show-toplevel"], { cwd: root, encoding: "utf8" }).trim();
  const { entries, lockFiles } = collectLockedRightKit(root, owned, { extraLocks: rightKitLockInventory({ root, config: {} }) });
  const distinct = new Map(entries.map((e) => [`${e.ecosystem}\0${e.name}\0${e.version}`, e]));
  const rows = [...distinct.values()].map((e) => ({ ecosystem: e.ecosystem, name: e.name, version: e.version, lockDigest: e.digest.value, lockFiles: [path.relative(root, e.lockFile)], registryDigest: e.digest.value, verifiedAt: "2026-10-23T00:00:00.000Z", via: "live" }));
  const latest = [...new Map(rows.map((r) => [`${r.ecosystem}\0${r.name}`, { ecosystem: r.ecosystem, name: r.name, latest: r.version, observedAt: "2026-10-23T00:00:00.000Z", via: "live" }])).values()];
  const snapshot = { schema: 1, mode: "admission", observedAt: "2026-10-23T00:00:00.000Z", registries: {}, policyRevision: "test", lockFiles: lockFiles.map((f) => path.relative(root, f)), entries: rows, latest };
  mutate?.(snapshot);
  snapshot.digest = snapshotDigest(snapshot);
  const releaseId = `${config.app}-${config.version}-${commit.slice(0, 8)}`;
  const stateRoot = tempRoot("right-release-state-");
  writeSnapshot(path.join(stateRoot, SNAPSHOT_FILE), snapshot);
  writeFileSync(path.join(stateRoot, "release-inputs.lock.json"), JSON.stringify({ schema: 1, releaseId, commit, platform }));
  writeAdmissionRecord(stateRoot, { releaseId, commit, platform, snapshot });
  void repoRoot;
  return { RIGHT_RELEASE_STATE_ROOT: stateRoot, RIGHT_RELEASE_RIGHTKIT_SNAPSHOT: path.join(stateRoot, SNAPSHOT_FILE) };
}

/** Valid, versioned registry evidence for a hand-built sealed manifest fixture (no RightKit pins). */
export function registryEvidenceFixture() {
  const snapshot = { schema: 1, mode: "admission", observedAt: "2026-10-23T00:00:00.000Z", registries: {}, policyRevision: "test", lockFiles: [], entries: [], latest: [] };
  snapshot.digest = snapshotDigest(snapshot);
  return registryManifestFields(snapshot, "0.6.0");
}
