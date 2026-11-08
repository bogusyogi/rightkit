// A unit-list run (reset plan step 5) plans ONE scan set (the plan's rows plus one scan per publisher requirement they do not prove) and runs it with runScans' own class B
// grouping; judgeUnitList answers per requirement and per unit. These tests replace unit-list-compile.test.mjs (the grouped publisher scheduler, deleted).
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { assertCanonicalRequirements, blockedUnitsOf, compileProofKey, judgeUnitList, planUnitListScans, publisherRequirementScan, runScans } from "./deterministic-scans.mjs";
import { oneOutput } from "./cross-targets.mjs";
import { proveWorkspaceCwd } from "./local-publish.mjs";

const requirement = (id, argv, options = {}) => {
  const body = { workspace: options.workspace ?? ".", cwd: options.cwd ?? ".", executable: options.executable ?? "cargo", argv, env: options.env ?? {}, phase: options.phase ?? "compile" };
  return { id, ...body, coverageKey: createHash("sha256").update(JSON.stringify(body)).digest("hex") };
};
const adapt = (r) => publisherRequirementScan(r, { batch: true });
const checkOf = (id, crate, options) => requirement(id, ["check", "-p", crate, "--message-format=json-render-diagnostics", "--all-targets"], options);
const clippyOf = (id, crate, options) => requirement(id, ["clippy", "-p", crate, "--message-format=json-render-diagnostics", "--no-deps", "--all-targets", "--", "-D", "warnings"], options);
const entry = (selector, ...requirements) => ({ selector, requirements });

test("an altered requirement refuses planning; ids are unique", () => {
  const row = requirement("a", ["test", "-p", "a"], { phase: "test" });
  row.argv.push("--no-run");
  assert.throws(() => assertCanonicalRequirements([entry("cargo:a", row)]), /invalid or duplicate/);
  const one = checkOf("x", "a");
  assert.throws(() => assertCanonicalRequirements([entry("cargo:a", one), entry("cargo:b", one)]), /invalid or duplicate/);
  assert.throws(() => assertCanonicalRequirements([]), /needs compile requirements/);
  assert.throws(() => assertCanonicalRequirements([{ selector: "cargo:a", requirements: [] }]), /nonempty canonical compile plan/);
});

test("compatible publisher rows of different crates share one configuration key; features, other commands and npm stay isolated", () => {
  const a = planUnitListScans({ entries: [entry("cargo:a", checkOf("a", "a")), entry("cargo:b", checkOf("b", "b")), entry("cargo:c", checkOf("c", "c", { workspace: "other" }))], adapt }).scans;
  assert.equal(a.length, 3);
  assert.equal(a[0].batchable.key, a[1].batchable.key, "same workspace, directory, environment and arguments but the crate");
  assert.notEqual(a[0].batchable.key, a[2].batchable.key, "another workspace never joins");
  assert.equal(a[0].batchable.crate, "a");
  assert.equal(a[0].batchable.target, a[0].args[a[0].args.indexOf("--target") + 1], "the row's own target is named for the feature partition");
  const isolated = planUnitListScans({ entries: [
    entry("cargo:a", requirement("f", ["check", "-p", "a", "--features", "extra"]), requirement("u", ["check", "-p", "a", "-p", "b"])),
    entry("npm:@rightkit/n", requirement("n", ["-r", "run", "build"], { executable: "pnpm" })),
  ], adapt }).scans;
  assert.deepEqual(isolated.map((s) => s.batchable), [undefined, undefined, undefined], "crate-specific features, two packages and pnpm are never batchable");
  const test = (extra) => publisherRequirementScan(requirement("t", ["test", "-p", "a", "--no-fail-fast", "--", "--show-output"], { phase: "test" }), { batch: true, ...extra });
  assert.equal(test({}).batchable, undefined, "a test row whose executables run inside the broker never groups");
  assert.ok(test({ cargoCommand: ["rustright", "cargo"], cargoLiveRoot: "/checkout", targetDirs: { ".": "/target" } }).batchable, "a test row whose executables run outside the broker groups (runScans' test group)");
  assert.equal(publisherRequirementScan(checkOf("a", "a")).batchable, undefined, "batching is the caller's choice");
});

test("the same requirement of several units is one scan; every member id maps to it", () => {
  const build = (id) => requirement(id, ["-r", "run", "build"], { executable: "pnpm" });
  const planned = planUnitListScans({ entries: [entry("npm:@rightkit/a", build("a")), entry("npm:@rightkit/b", build("b"))], adapt });
  assert.equal(planned.scans.length, 1);
  assert.equal(planned.scanOf.get("a"), "a"); assert.equal(planned.scanOf.get("b"), "a");
});

test("a requirement that moved to its workspace root is planned there, so it groups; the proof still refuses an override", () => {
  const a = requirement("a", ["check", "--profile", "release", "--target", "aarch64-apple-darwin", "-p", "rightkit-a", "--all-targets"], { cwd: "crates/rightkit-a" });
  const b = requirement("b", ["check", "--profile", "release", "--target", "aarch64-apple-darwin", "-p", "rightkit-b", "--all-targets"], { cwd: "crates/rightkit-b" });
  const prove = (r, exists = () => false) => proveWorkspaceCwd({ requirement: r, snapshot: "/snapshot", checkout: "/checkout", exists, toolchainOf: () => "stable-x86_64" });
  assert.equal(prove(a).normalized, true);
  assert.equal(prove(b, (p) => p === "/snapshot/crates/rightkit-b/.cargo/config.toml").normalized, false);
  const planned = planUnitListScans({ entries: [entry("cargo:a", a), entry("cargo:b", b)], adapt, normalizedCwds: new Map([["a", prove(a).cwd], ["b", prove(b).cwd]]) });
  assert.deepEqual(planned.scans.map((s) => s.cwd), [".", "."]);
  assert.equal(planned.scans[0].batchable.key, planned.scans[1].batchable.key);
  assert.equal(proveWorkspaceCwd({ requirement: a, snapshot: "/snapshot", checkout: "/checkout", exists: () => false, toolchainOf: (p) => p.endsWith("rightkit-a") ? "other" : "stable" }).normalized, false);
  assert.match(proveWorkspaceCwd({ requirement: a, snapshot: "/snapshot", checkout: "/checkout", exists: () => false, toolchainOf: () => ({ error: "exit-125" }) }).reason, /snapshot workspace \(exit-125\)/);
});

test("NN27: a plan row that compiles the same crate, features, target and environment stands for the publisher check and clippy; nothing else does", () => {
  // planScans' own rows go through oneOutput like the publisher rows (release profile, the host's one target)
  const plannedRow = (id, argv) => ({ id, class: "B", tool: "rustright", cwd: ".", env: {}, args: ["cargo", ...oneOutput(argv, { platform: process.platform })] });
  const plannedClippy = plannedRow("clippy:a", ["clippy", "-p", "a", "--no-deps", "--all-targets", "--", "-D", "warnings"]);
  const plannedCheck = plannedRow("check:b", ["check", "-p", "b", "--all-targets", "--locked"]);
  const adaptWrapped = (r) => ({ ...adapt(r), args: ["cargo", ...adapt(r).args], tool: "rustright" });
  const planned = planUnitListScans({
    entries: [entry("cargo:a", checkOf("a-check", "a"), clippyOf("a-clippy", "a"), requirement("a-lints", ["clippy", "-p", "a", "--no-deps", "--all-targets", "--", "-D", "clippy::pedantic"])),
      entry("cargo:b", clippyOf("b-clippy", "b"), checkOf("b-check", "b"), requirement("b-features", ["check", "-p", "b", "--all-targets", "--features", "x"]))],
    planned: [plannedClippy, plannedCheck], adapt: adaptWrapped,
  });
  assert.equal(planned.providerOf.get("a-check"), "clippy:a", "clippy proves the check");
  assert.equal(planned.providerOf.get("a-clippy"), "clippy:a", "the same lints prove the clippy");
  assert.equal(planned.providerOf.get("b-check"), "check:b", "a check proves the check");
  assert.equal(planned.providerOf.has("a-lints"), false, "other lints are another compile");
  assert.equal(planned.providerOf.has("b-clippy"), false, "a check never proves a clippy");
  assert.equal(planned.providerOf.has("b-features"), false, "other features are another compile");
  assert.deepEqual(planned.scans.map((s) => s.id), ["a-lints", "b-clippy", "b-features"]);
  assert.deepEqual([...planned.provers].sort(), ["check:b", "clippy:a"]);
  assert.equal(compileProofKey({ args: ["cargo", "doc", "-p", "a"] }), null, "docs and tests never reuse");
  assert.equal(compileProofKey({ args: ["check", "-p", "a", "--weird"] }), null, "an unknown flag proves nothing");
});

test("a unit that failed an internal requirement, and every unit that depends on it, gets no scan and is reported blocked", async () => {
  const dependencies = { "cargo:a": [], "cargo:b": ["cargo:a"], "cargo:c": [] };
  const blocked = blockedUnitsOf({ dependencies, initialFailures: new Set(["cargo:a"]) });
  assert.deepEqual([...blocked].sort(), ["cargo:a", "cargo:b"]);
  const entries = [entry("cargo:a", checkOf("a", "a")), entry("cargo:b", checkOf("b", "b")), entry("cargo:c", checkOf("c", "c"))];
  const planned = planUnitListScans({ entries, adapt, blocked });
  assert.deepEqual(planned.scans.map((s) => s.id), ["c"]);
  const rows = new Map([["c", { id: "c", status: "pass", command: "cargo check" }]]);
  const results = await judgeUnitList({ entries, dependencies, blocked, ...planned, rowOf: (id) => rows.get(id), verify: async () => ({ ok: true, evidenceHash: "e".repeat(64) }) });
  assert.deepEqual(results.map((r) => [r.selector, r.status]), [["cargo:a", "blocked"], ["cargo:b", "blocked"], ["cargo:c", "passed"]]);
});

test("a requirement passes only on a passing row the publisher verifies; a unit with a failed dependency is blocked as a whole", async () => {
  const dependencies = { "cargo:a": [], "cargo:b": ["cargo:a"], "cargo:c": ["cargo:b"], "cargo:d": [] };
  const entries = [entry("cargo:a", checkOf("a1", "a"), checkOf("a2", "a", { cwd: "crates/a" })), entry("cargo:b", checkOf("b1", "b")), entry("cargo:c", checkOf("c1", "c")), entry("cargo:d", checkOf("d1", "d"), checkOf("d2", "d", { workspace: "w" }))];
  const planned = planUnitListScans({ entries, adapt });
  const rows = new Map(planned.scans.map((s) => [s.id, { id: s.id, status: s.id === "a2" ? "fail" : "pass", command: s.command }]));
  rows.set("d2", { id: "d2", status: "pass", command: "cargo check" });
  const seen = [];
  const verify = async ({ row, member, attempts, requirement: r }) => { seen.push([member.id, r.id, attempts.length]); return { ok: member.id !== "d2", evidenceHash: createHash("sha256").update(member.id).digest("hex") }; };
  const results = await judgeUnitList({ entries, dependencies, ...planned, rowOf: (id) => rows.get(id), verify });
  const status = (id) => results.find((r) => r.id === id).status;
  assert.equal(status("a1"), "passed", "a unit with a failed row of its own keeps its other passed rows");
  assert.equal(status("a2"), "failed");
  assert.equal(status("b1"), "blocked", "its dependency has a failed row: blocked, never passed");
  assert.equal(status("c1"), "blocked", "and so is the dependent of a blocked unit");
  assert.equal(status("d1"), "passed"); assert.equal(status("d2"), "failed", "a row the publisher does not verify is failed even when it passed");
  assert.equal(results.find((r) => r.id === "b1").evidenceHash, undefined);
  assert.equal(results.length, 6, "one verdict per requirement");
  assert.ok(seen.every(([id, same, n]) => id === same && n === 1), "the verifier sees the row's attempt");
  const missing = await judgeUnitList({ entries: [entry("cargo:a", checkOf("a", "a"))], ...planUnitListScans({ entries: [entry("cargo:a", checkOf("a", "a"))], adapt }), rowOf: () => undefined, verify: async () => ({ ok: true, evidenceHash: "f".repeat(64) }) });
  assert.equal(missing[0].status, "failed", "a requirement whose row never came back is failed");
});

test("a requirement a plan row proves is judged on that row and the receipt names it", async () => {
  const plannedClippy = { id: "clippy:a", class: "B", tool: "rustright", cwd: ".", env: {}, args: ["cargo", ...oneOutput(["clippy", "-p", "a", "--no-deps", "--all-targets", "--", "-D", "warnings"], { platform: process.platform })] };
  const entries = [entry("cargo:a", checkOf("a-check", "a"))];
  const planned = planUnitListScans({ entries, planned: [plannedClippy], adapt: (r) => ({ ...adapt(r), args: ["cargo", ...adapt(r).args] }) });
  assert.equal(planned.scans.length, 0);
  const results = await judgeUnitList({ entries, ...planned, rowOf: (id) => (id === "clippy:a" ? { id, status: "pass", command: "rustright cargo clippy", snapshotEvidence: { receiptSha256: "a".repeat(64) } } : undefined), verify: async ({ row, attempts }) => ({ ok: /^[0-9a-f]{64}$/.test(row.snapshotEvidence.receiptSha256) && attempts[0].provedBy === "clippy:a", evidenceHash: "b".repeat(64) }) });
  assert.deepEqual(results, [{ selector: "cargo:a", id: "a-check", coverageKey: entries[0].requirements[0].coverageKey, providedBy: "clippy:a", status: "passed", evidenceHash: "b".repeat(64) }]);
});

// runScans' own class B grouping runs publisher rows: two crates' checks are one cargo invocation, each member keeps its own row.
test("runScans groups publisher rows of one configuration into one invocation and keeps every member's own row", async () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "rk-unit-list-")); const target = path.join(root, "target"); fs.mkdirSync(target, { recursive: true });
  fs.writeFileSync(path.join(root, "Cargo.toml"), "[workspace]\nmembers = []\n");
  const calls = [];
  const spawn = async (cmd, args) => {
    calls.push({ cmd, args: [...args] });
    if (args.includes("--version")) return { status: 0, stdout: "cargo 1.98.0 (test)\n", stderr: "" };
    if (args.includes("tree")) return { status: 0, stdout: "", stderr: "" };
    const crates = args.flatMap((a, i) => (a === "-p" ? [args[i + 1]] : []));
    const lines = crates.map((c) => ({ reason: "compiler-artifact", package_id: `path+file:///x/${c}#${c}@0.1.0`, manifest_path: path.join(root, "Cargo.toml"), target: { name: c, kind: ["lib"] }, profile: { test: false }, executable: null }));
    lines.push({ reason: "build-finished", success: true });
    return { status: 0, stdout: lines.map((l) => JSON.stringify(l)).join("\n") + "\n", stderr: "" };
  };
  const scans = planUnitListScans({ entries: [entry("cargo:a", checkOf("pub-a", "a")), entry("cargo:b", checkOf("pub-b", "b"))], adapt: (r) => publisherRequirementScan(r, { batch: true, targetDirs: { ".": target } }) }).scans;
  const plan = { schema: "rightkit.deterministic-scans/1", roots: { contentRoot: root, liveRoot: root }, scans, notes: [], deferred: [], qualifyOwed: [] };
  const result = await runScans(plan, { classes: ["B"], classAEarlier: true, jobs: 4, classBJobs: 4, spawn, classBBatching: true });
  const compiles = calls.filter((c) => c.args[0] === "check");
  assert.equal(compiles.length, 1, JSON.stringify(result.scans.map((r) => [r.id, r.status, r.detail])));
  assert.deepEqual(compiles[0].args.filter((a, i, all) => a === "-p" || all[i - 1] === "-p"), ["-p", "a", "-p", "b"]);
  for (const id of ["pub-a", "pub-b"]) {
    const row = result.scans.find((r) => r.id === id);
    assert.equal(row.status, "pass", JSON.stringify(row)); assert.ok(row.batchedWith, `${id} ran in the group`);
  }
});
