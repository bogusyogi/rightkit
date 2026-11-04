import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import test from "node:test";
import { planUnitListCompile, proveWorkspaceCwd } from "./unit-list-compile.mjs";

const requirement = (id, argv, options = {}) => {
  const body = { workspace: options.workspace ?? ".", cwd: options.cwd ?? ".", executable: options.executable ?? "cargo", argv, env: options.env ?? {}, phase: options.phase ?? "compile" };
  return { id, ...body, coverageKey: createHash("sha256").update(JSON.stringify(body)).digest("hex") };
};

test("union plan combines compatible commands while retaining every required unit identity", () => {
  const entries = [
    { selector: "cargo:rightkit-a", requirements: [requirement("a", ["check", "-p", "rightkit-a", "--locked"])] },
    { selector: "cargo:rightkit-b", requirements: [requirement("b", ["check", "-p", "rightkit-b", "--locked"])] },
    { selector: "cargo:rightkit-c", requirements: [requirement("c", ["check", "-p", "rightkit-c", "--locked"], { workspace: "other" })] },
  ];
  const jobs = planUnitListCompile(entries);
  assert.equal(jobs.length, 2);
  assert.deepEqual(jobs[0].argv, ["check", "-p", "rightkit-a", "-p", "rightkit-b", "--locked"]);
  assert.deepEqual(jobs[0].members.map((m) => m.id), ["a", "b"]);
});

test("identical npm work runs once; feature and cwd differences remain isolated", () => {
  const build = (id) => requirement(id, ["-r", "run", "build"], { executable: "pnpm" });
  const jobs = planUnitListCompile([
    { selector: "npm:@rightkit/a", requirements: [build("a"), requirement("c", ["check", "-p", "a", "--features", "extra"])] },
    { selector: "npm:@rightkit/b", requirements: [build("b"), requirement("d", ["check", "-p", "b", "--features", "extra"]), requirement("e", ["check", "-p", "b"], { cwd: "crates/b" })] },
  ]);
  assert.equal(jobs.length, 4);
  assert.deepEqual(jobs[0].members.map((m) => m.id), ["a", "b"]);
});

test("an altered requirement hash refuses planning", () => {
  const row = requirement("a", ["test", "-p", "a"], { phase: "test" });
  row.argv.push("--no-run");
  assert.throws(() => planUnitListCompile([{ selector: "cargo:a", requirements: [row] }]), /invalid or duplicate/);
});

test("verified workspace cwd combines package-selected checks and rejects intervening overrides", () => {
  const a = requirement("a", ["check", "--profile", "release", "--target", "aarch64-apple-darwin", "-p", "rightkit-a", "--all-targets"], { cwd: "crates/rightkit-a" });
  const b = requirement("b", ["check", "--profile", "release", "--target", "aarch64-apple-darwin", "-p", "rightkit-b", "--all-targets"], { cwd: "crates/rightkit-b" });
  const prove = (r, exists = () => false) => proveWorkspaceCwd({ requirement: r, snapshot: "/snapshot", checkout: "/checkout", exists, toolchainOf: () => "stable-x86_64" });
  assert.equal(prove(a).normalized, true);
  assert.equal(prove(b, (p) => p === "/snapshot/crates/rightkit-b/.cargo/config.toml").normalized, false);
  const jobs = planUnitListCompile([{ selector: "cargo:a", requirements: [a] }, { selector: "cargo:b", requirements: [b] }], { normalizedCwds: new Map([["a", prove(a).cwd], ["b", prove(b).cwd]]) });
  assert.equal(jobs.length, 1);
  assert.equal(jobs[0].cwd, ".");
  assert.deepEqual(jobs[0].argv, ["check", "--profile", "release", "--target", "aarch64-apple-darwin", "-p", "rightkit-a", "-p", "rightkit-b", "--all-targets"]);
  assert.equal(proveWorkspaceCwd({ requirement: a, snapshot: "/snapshot", checkout: "/checkout", exists: () => false, toolchainOf: (p) => p.endsWith("rightkit-a") ? "other" : "stable" }).normalized, false);
  assert.match(proveWorkspaceCwd({ requirement: a, snapshot: "/snapshot", checkout: "/checkout", exists: () => false, toolchainOf: () => ({ error: "exit-125" }) }).reason, /snapshot workspace \(exit-125\)/);
});
