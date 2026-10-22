import assert from "node:assert/strict";
import test from "node:test";
import { allowsetsFor, dispatchRefusal, parseArgs, satisfies, waves } from "./rk-pipeline.mjs";

const SHA = "f".repeat(40);

test("argv: command, units, registry and jobs", () => {
  assert.throws(() => parseArgs(["bogus"]), /command must be/);
  assert.throws(() => parseArgs(["ready", "--registry", "pypi"]), /crates or npm/);
  assert.throws(() => parseArgs(["preflight", "x", "--jobs", "0"]), /positive integer/);
  const opts = parseArgs(["dispatch", "rightkit-daemon", "rightkit-crash", "--jobs", "3"]);
  assert.deepEqual(opts.units, ["rightkit-daemon", "rightkit-crash"]);
  assert.deepEqual(parseArgs(["preflight", "rightkit-daemon rightkit-crash "]).units, ["rightkit-daemon", "rightkit-crash"], "a single space-joined argument is split");
  assert.equal(opts.jobs, 3);
  assert.equal(opts.registry, "crates");
});

test("caret requirements against the registry: 0.x pins the minor, >=1 pins the major", () => {
  assert.equal(satisfies("0.5.0", "0.5.4"), true);
  assert.equal(satisfies("0.5.5", "0.5.4"), false);
  assert.equal(satisfies("0.4.0", "0.3.0"), false);
  assert.equal(satisfies("1.2.0", "1.3.1"), true);
  assert.equal(satisfies("1.2.0", "2.0.0"), false);
  assert.equal(satisfies("0.5.0", null), false);
});

test("waves: a unit is ready when every unpublished dependency's requirement is already on the registry", () => {
  const crates = new Map([
    ["rightkit-child-process", { name: "rightkit-child-process", version: "0.5.5", publish: true, deps: [] }],
    ["rightkit-daemon", { name: "rightkit-daemon", version: "0.4.0", publish: true, deps: [{ name: "rightkit-child-process", req: "0.5.0" }] }],
    ["rightkit-models", { name: "rightkit-models", version: "0.5.1", publish: true, deps: [{ name: "rightkit-daemon", req: "0.4.0" }] }],
    ["rightkit-browser", { name: "rightkit-browser", version: "0.3.5", publish: true, deps: [{ name: "rightkit-models", req: "0.5.0" }, { name: "rightkit-child-process", req: "0.5.4" }] }],
    ["rightkit-private", { name: "rightkit-private", version: "9.9.9", publish: false, deps: [] }],
    ["rightkit-live", { name: "rightkit-live", version: "1.0.0", publish: true, deps: [] }],
  ]);
  const registry = new Map([["rightkit-child-process", "0.5.4"], ["rightkit-daemon", "0.3.0"], ["rightkit-models", "0.4.4"], ["rightkit-browser", "0.3.4"], ["rightkit-live", "1.0.0"]]);
  const r = waves(crates, registry);
  assert.deepEqual([...r.unpublished].sort(), ["rightkit-browser", "rightkit-child-process", "rightkit-daemon", "rightkit-models"]);
  assert.deepEqual(r.waves, [["rightkit-child-process", "rightkit-daemon"], ["rightkit-models"], ["rightkit-browser"]], "daemon needs ^0.5.0 of child-process, which 0.5.4 satisfies: wave 1");
  const cycle = waves(new Map([["a", { name: "a", version: "1.0.0", publish: true, deps: [{ name: "b", req: "1.0.0" }] }], ["b", { name: "b", version: "1.0.0", publish: true, deps: [{ name: "a", req: "1.0.0" }] }]]), new Map());
  assert.deepEqual(cycle.waves, [{ stuck: ["a", "b"] }]);
});

test("dispatch refusal: only a green receipt for this exact commit, or a CI-routed one with green scans, may dispatch", () => {
  assert.match(dispatchRefusal(null, SHA), /no pre-flight receipt/);
  assert.match(dispatchRefusal({ sha: "a".repeat(40), ok: true }, SHA), /HEAD is/);
  assert.match(dispatchRefusal({ sha: SHA, ok: false, failedAt: "doc" }, SHA), /red at doc/);
  assert.equal(dispatchRefusal({ sha: SHA, ok: true }, SHA), null);
  assert.equal(dispatchRefusal({ sha: SHA, ok: false, route: "ci", scans: { ok: true } }, SHA), null);
  assert.match(dispatchRefusal({ sha: SHA, ok: false, route: "ci", scans: { ok: false } }, SHA), /scans did not pass/);
});

test("allowsets are derived from the planner's own refusals and converge", async () => {
  const calls = [];
  const planFn = async (req) => {
    calls.push(req);
    const errors = [];
    if (!req.allowHostExclusionText.includes("rightkit-overlay-win")) errors.push("rightkit-overlay-win: ci/gate.config.json restricts it to windows, so macOS verification would skip it; name it in allow_host_exclusion to publish it");
    if (!req.allowMissingAssetsText.includes("rightkit-models")) errors.push("rightkit-models: its tests need assets this workflow does not provision (parakeet); name it in allow_missing_assets to publish it with those tests skipped");
    if (errors.length) throw new Error(`publish request refused:\n  - ${errors.join("\n  - ")}`);
    return { units: [] };
  };
  const { allow, errors } = await allowsetsFor("crates", ["rightkit-overlay-win", "rightkit-models", "rightkit-daemon"], planFn);
  assert.deepEqual(errors, []);
  assert.deepEqual(allow.allow_host_exclusion, ["rightkit-overlay-win"]);
  assert.deepEqual(allow.allow_missing_assets, ["rightkit-models"]);
  assert.ok(calls.length <= 3);
  const stuck = await allowsetsFor("crates", ["x"], async () => { throw new Error("publish request refused:\n  - x: version \"1.0\" is not a plain x.y.z release version"); });
  assert.deepEqual(stuck.errors, ["x: version \"1.0\" is not a plain x.y.z release version"]);
});
