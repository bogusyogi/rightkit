// GC-A27 NN07: a row whose inputs did not change does no work. The sidecar's cargo build was sent to the broker on every run, even for an edit in another
// workspace (acceptance case 3 on gate 0.6.63, 2026-11-24; rightkit#1514). The gate now keys the sidecar's own sources from the commit and, when they are
// those of the last staging, only has the staged files checked.
import assert from "node:assert/strict";
import { test } from "node:test";
import { sidecarSourceDirs, sidecarSourceKey } from "./bin/rightkit-gate.mjs";
import { isRememberedRead } from "./deterministic-scans.mjs";

const C = "c".repeat(40);
/** A fake git over a tree of { path: content }; blob ids are derived from content so an edit changes the id. */
function gitOf(tree) {
  const oid = (s) => Buffer.from(s).toString("hex").padEnd(40, "0").slice(0, 40);
  const blobs = new Map(Object.values(tree).map((s) => [oid(s), s]));
  return (...a) => {
    if (a[0] === "ls-tree") return { status: 0, stdout: Object.entries(tree).map(([p, s]) => `100644 blob ${oid(s)}\t${p}`).join("\0") + "\0" };
    if (a[0] === "cat-file") return blobs.has(a[2]) ? { status: 0, stdout: blobs.get(a[2]) } : { status: 128, stdout: "" };
    return { status: 1, stdout: "" };
  };
}
const engine = '[package]\nname = "engine"\n[lib]\npath = "src/lib.rs"\n[dependencies]\nshared = { path = "../shared" }\n';
const base = {
  "app/right-release.config.mjs": "export default {}",
  "app/engine/Cargo.toml": engine, "app/engine/src/lib.rs": "pub fn a() {}", "app/engine/Cargo.lock": "lock",
  "app/shared/Cargo.toml": '[package]\nname = "shared"\n', "app/shared/src/lib.rs": "pub fn s() {}",
  "rk/crates/rk-leaf/Cargo.toml": '[package]\nname = "rk-leaf"\n', "rk/crates/rk-leaf/src/lib.rs": "pub fn leaf() {}",
  "rust-toolchain.toml": "[toolchain]\nchannel = \"1.99\"\n",
};
const result = { files: [{ kind: "cargo-bin", package: "engine", path: "src-tauri/binaries/engine.exe" }] };
const keyOf = (tree, over = {}) => { const git = gitOf(tree); const dirs = sidecarSourceDirs({ git, commit: C, result }); return sidecarSourceKey({ git, commit: C, config: "app/right-release.config.mjs", target: "host", mode: "release", dirs, env: {}, gateIdentity: "g", toolchains: [["stable", 1, 1]], ...over }); };

test("the sources are the staged package's crate and its path dependencies, never a [lib] or [[bin]] source path", () => {
  assert.deepEqual(sidecarSourceDirs({ git: gitOf(base), commit: C, result }), ["app/engine", "app/shared"]);
});
test("the key holds for an edit elsewhere and changes for an edit the sidecar reads", () => {
  const k = keyOf(base);
  assert.equal(keyOf({ ...base, "rk/crates/rk-leaf/src/lib.rs": "pub fn leaf() { 1; }" }), k, "an edit in another workspace leaves the key");
  for (const [file, body] of [["app/engine/src/lib.rs", "pub fn b() {}"], ["app/shared/src/lib.rs", "pub fn t() {}"], ["app/engine/Cargo.lock", "lock2"], ["rust-toolchain.toml", "x"], ["app/right-release.config.mjs", "export default { x: 1 }"]])
    assert.notEqual(keyOf({ ...base, [file]: body }), k, `an edit to ${file} changes the key`);
  assert.notEqual(keyOf(base, { mode: "dev" }), k); assert.notEqual(keyOf(base, { target: "aarch64-apple-darwin" }), k);
  assert.notEqual(keyOf(base, { gateIdentity: "h" }), k); assert.notEqual(keyOf(base, { env: { RUSTFLAGS: "-C x" } }), k);
  // folders and wrappers the run picks do not change what Cargo builds, and change on every run: they are not in the key
  assert.equal(keyOf(base, { env: { CARGO_TARGET_DIR: "D:\\t\\run-1", CARGO_HOME: "D:\\h", RUSTC_WRAPPER: "sccache", CARGO: "D:\\shim-1\\cargo.exe" } }), k); assert.notEqual(keyOf(base, { toolchains: [["stable", 1, 2]] }), k);
});
test("nothing is skipped when a staged file is not built by Cargo, its package is not found, or a path dependency is not a committed crate", () => {
  const git = gitOf(base);
  assert.equal(sidecarSourceDirs({ git, commit: C, result: { files: [{ kind: "download", path: "x" }] } }), null);
  assert.equal(sidecarSourceDirs({ git, commit: C, result: { files: [{ kind: "cargo-bin", package: "missing" }] } }), null);
  assert.equal(sidecarSourceDirs({ git: gitOf({ ...base, "app/engine/Cargo.toml": engine.replace("../shared", "../nowhere") }), commit: C, result }), null);
});
test("the commit's own listing and blobs, read by full id, are remembered within a run", () => {
  assert.equal(isRememberedRead("git", ["ls-tree", "-r", "-z", C]), true);
  assert.equal(isRememberedRead("git", ["ls-tree", "-r", "--name-only", "-z", C]), true);
  assert.equal(isRememberedRead("git", ["cat-file", "-p", C]), true);
  assert.equal(isRememberedRead("git", ["ls-tree", "-r", "-z", "HEAD"]), false);
  assert.equal(isRememberedRead("git", ["ls-tree", C, "--", "a"]), false);
});
test("sidecar staging is told its build mode; a caller's RIGHTKIT_BUILD_MODE never reaches it (the key names the mode the gate chose)", async () => {
  const { readFileSync } = await import("node:fs"); const src = readFileSync(new URL("./bin/rightkit-gate.mjs", import.meta.url), "utf8");
  assert.match(src, /RIGHTKIT_BUILD_MODE: stage === "dev" \? "dev" : "release" \}/);
  assert.doesNotMatch(src, /\.\.\.\(stage === "dev" \? \{ RIGHTKIT_BUILD_MODE: "dev" \} : \{\}\)/);
});
test("the staged files are compared by path and sha256, not by when the receipt was written", async () => {
  const fs = await import("node:fs"); const os = await import("node:os"); const path = await import("node:path");
  const { stagedFilesDigest } = await import("./bin/rightkit-gate.mjs");
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "rk-receipt-")); const f = path.join(dir, "stage-sidecar.json");
  const files = [{ path: "src-tauri/binaries/e.exe", sha256: "a".repeat(64) }];
  fs.writeFileSync(f, JSON.stringify({ stagedAt: "2026-11-24T10:00:00Z", files })); const one = stagedFilesDigest(f);
  fs.writeFileSync(f, JSON.stringify({ stagedAt: "2026-11-24T11:00:00Z", files })); assert.equal(stagedFilesDigest(f), one, "a new time is the same staging");
  fs.writeFileSync(f, JSON.stringify({ stagedAt: "2026-11-24T11:00:00Z", files: [{ ...files[0], sha256: "b".repeat(64) }] })); assert.notEqual(stagedFilesDigest(f), one, "another binary is not");
  fs.writeFileSync(f, "{"); assert.equal(stagedFilesDigest(f), null);
  fs.rmSync(dir, { recursive: true, force: true });
});
test("a staging the sync removed is put back from its kept copy when the sources match, and kept after each staging", async () => {
  const src = (await import("node:fs")).readFileSync(new URL("./bin/rightkit-gate.mjs", import.meta.url), "utf8");
  // put back: only when the key matches and the receipt is gone from the snapshot
  assert.match(src, /keyOf\(nowParts\) === prior\.key && Array\.isArray\(prior\.kept\) && typeof prior\.receipt === "string" && !fs\.existsSync\(path\.join\(ds\.path, prior\.receipt\)\)/);
  // kept: the receipt and every staged file, recorded in the state
  assert.match(src, /const files = \[receiptAbs, \.\.\.\(Array\.isArray\(result\?\.files\)/); assert.match(src, /\.\.\.\(kept \? \{ kept \} : \{\}\)/);
});
