// Pure affected-unit, grouping and scan-planning regressions.
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { buildInventory, classifyUnitResults, selectAffectedAcrossInventories, selectAffectedUnits, unitListReport } from "./affected-units.mjs";
import { privateUnitManifest } from "./bin/rightkit-gate.mjs";
import { scopeScans } from "./app-closure.mjs";
import { batchClippyInvocations } from "./local-publish.mjs";
import { planScans } from "./deterministic-scans.mjs";

const inventory = () => buildInventory({
  cargo: [
    { dir: "crates/rightkit-fs", name: "rightkit-fs", deps: [] },
    { dir: "crates/rightkit-db", name: "rightkit-db", deps: ["rightkit-fs"] },
    { dir: "crates/rightkit-app", name: "rightkit-app", deps: ["rightkit-db", "rightkit-fs"] },
    { dir: "crates/other", name: "other", deps: [] },
  ],
  npm: [{ dir: "packages/release", name: "@rightkit/release", deps: [] }],
});

test("selectAffectedUnits: changed crate plus transitive dependents, dependency-first, unmatched named", () => {
  const inv = inventory();
  assert.equal(inv.skipped.length, 1); // `other` cannot form a cargo:rightkit-* selector
  const { selected, unmatched } = selectAffectedUnits({ changedPaths: ["crates/rightkit-fs/src/lib.rs", "docs/note.md"], inventory: inv });
  assert.deepEqual(selected.map((s) => s.selector), ["cargo:rightkit-fs", "cargo:rightkit-db", "cargo:rightkit-app"]);
  assert.equal(selected[0].reason, "changed");
  assert.equal(selected[1].reason, "dependent");
  assert.equal(selected[1].via, "cargo:rightkit-fs");
  assert.deepEqual(unmatched, ["docs/note.md"]);
});

test("selectAffectedUnits: a shared workspace path selects its whole kind with the reason named", () => {
  const inv = inventory();
  inv.skipped = [];
  const rust = selectAffectedUnits({ changedPaths: ["Cargo.lock"], inventory: inv });
  assert.deepEqual(rust.selected.filter((s) => s.kind === "cargo").map((s) => s.selector), ["cargo:rightkit-fs", "cargo:rightkit-db", "cargo:rightkit-app"]);
  assert.ok(rust.selected.every((s) => s.kind !== "cargo" || s.reason === "shared-rust-workspace"));
  const gate = selectAffectedUnits({ changedPaths: ["ci/gate.config.json"], inventory: inv });
  assert.equal(gate.selected.length, inv.units.length);
});

test("classifyUnitResults: a failed dependency blocks its dependents, never passes or fails them", () => {
  const inv = inventory();
  const { selected } = selectAffectedUnits({ changedPaths: ["crates/rightkit-fs/src/lib.rs"], inventory: inv });
  const results = classifyUnitResults({ selected, inventory: inv, outcomes: new Map([["cargo:rightkit-fs", { ran: true, code: 1 }]]) });
  assert.deepEqual(results.map((r) => r.status), ["failed", "blocked", "blocked"]);
  assert.equal(results[2].blockedBy, "cargo:rightkit-db");
  const report = unitListReport({ commit: "a".repeat(40), stage: "compile", selected, results });
  assert.equal(report.ok, false);
  assert.deepEqual(report.counts, { passed: 0, failed: 1, blocked: 2, "not-run": 0 });
});

test("batchClippyInvocations: compatible crates share one invocation; crate-specific features stay solo", () => {
  const invocations = batchClippyInvocations(["a", "b", "c", "d"], {
    featureSetsOf: (u) => (u === "c" ? [["--features", "x"]] : u === "d" ? [["--all-features"]] : [[]]),
    assetGatedOf: (u) => u === "d",
    crossTargetsOf: () => [],
  });
  assert.equal(invocations.length, 3);
  assert.deepEqual(invocations[0].crates, ["a", "b"]);
  assert.deepEqual(invocations[0].args.slice(0, 5), ["clippy", "-p", "a", "-p", "b"]);
  assert.deepEqual(invocations[1].crates, ["c"]);
  assert.deepEqual(invocations[2].crates, ["d"]);
});

test("planScans: dependency-freshness is off the blocking path unless advice is asked for", (t) => {
  const scratch = ((root) => (fs.existsSync(root) ? root : os.tmpdir()))(process.platform === "win32" ? "D:\\scratch" : "/Volumes/D/scratch");
  const root = fs.mkdtempSync(path.join(scratch, "rk-freshness-"));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  fs.mkdirSync(path.join(root, ".rightkit"), { recursive: true });
  fs.writeFileSync(path.join(root, ".rightkit", "update.json"), "{}\n");
  const base = { changed: [], contentRoot: root, rustUnits: [], npmUnits: [] };
  assert.equal(planScans(base).scans.some((s) => s.id === "dependency-freshness"), false);
  assert.equal(planScans({ ...base, freshnessAdvice: true }).scans.some((s) => s.id === "dependency-freshness"), true);
});


test("inventory refuses missing internal edges and unsupported ownership", () => {
  assert.throws(() => buildInventory({ npm: [{ dir: "packages/a", name: "@rightkit/a", deps: ["@rightkit/missing"] }] }), /unresolved internal dependencies/);
  assert.throws(() => selectAffectedUnits({ changedPaths: ["crates/other/lib.rs"], inventory: inventory() }), /unsupported unit/);
});

test("mixed old and current dependency edges reach a fixed point", () => {
  const oldInventory = buildInventory({ cargo: [
    { dir: "crates/rightkit-a", name: "rightkit-a", deps: [] },
    { dir: "crates/rightkit-b", name: "rightkit-b", deps: ["rightkit-a"] },
    { dir: "crates/rightkit-c", name: "rightkit-c", deps: [] },
  ] });
  const inventory = buildInventory({ cargo: [
    { dir: "crates/rightkit-a", name: "rightkit-a", deps: [] },
    { dir: "crates/rightkit-b", name: "rightkit-b", deps: [] },
    { dir: "crates/rightkit-c", name: "rightkit-c", deps: ["rightkit-b"] },
  ] });
  const result = selectAffectedAcrossInventories({ changedPaths: ["crates/rightkit-a/src/lib.rs"], oldInventory, inventory });
  assert.deepEqual(result.selected.map((u) => u.selector), ["cargo:rightkit-a", "cargo:rightkit-b", "cargo:rightkit-c"]);
  assert.equal(result.selected[2].via, "cargo:rightkit-b");
});


test("private unit identities permit check and compile planning without publication version constraints", () => {
  assert.deepEqual(privateUnitManifest({ selector: "npm:@rightkit/build-control-policy", content: '{"name":"@rightkit/build-control-policy","private":true,"version":"0.1.5-phase1"}' }), { kind: "npm", name: "@rightkit/build-control-policy", private: true });
  assert.deepEqual(privateUnitManifest({ selector: "cargo:rightkit-align", content: '[package]\nname = "rightkit-align"\npublish = false\n' }), { kind: "cargo", name: "rightkit-align", private: true });
  assert.throws(() => privateUnitManifest({ selector: "cargo:rightkit-align", content: '[package]\nname = "wrong"\n' }), /manifest name does not match/);
});


test("generic scoped planner keeps private unit Class B rows and excludes unrelated units", () => {
  const scans = [
    { id: "check:rightkit-align", class: "B" },
    { id: "check:rightkit-other", class: "B" },
    { id: "test:npm:rightkit-sync-worker", class: "B", kind: "test" },
    { id: "test:npm:rightkit-other", class: "B", kind: "test" },
  ];
  const rustUnits = [{ name: "rightkit-align", cargoDir: "." }, { name: "rightkit-other", cargoDir: "." }];
  const npmUnits = [{ name: "rightkit-sync-worker", dir: "packages/sync-worker" }, { name: "rightkit-other", dir: "packages/other" }];
  const closure = { dirs: ["crates/rightkit-align", "packages/sync-worker"], dependencyDirs: [], rust: new Set(["rightkit-align"]), npm: new Set(["rightkit-sync-worker"]) };
  const { scans: selected } = scopeScans(scans, closure, { rustUnits, npmUnits });
  assert.deepEqual(selected.map((s) => s.id), ["check:rightkit-align", "test:npm:rightkit-sync-worker"]);
});
