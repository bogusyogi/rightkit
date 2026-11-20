// NN42 through the real scan runner with a stand-in cargo: which compile commands a stage sends for rows that differ only by --target.
// Rows are shaped as the planner shapes them: the host's own row names its target before `-p` (oneOutput adds it when the row is planned),
// a cross-target row names its target after the crate's flags.
import assert from "node:assert/strict";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { batchKeyParts, fileScanCache, runScans } from "./deterministic-scans.mjs";
import { TARGET_SPLIT_NOTE, workspaceManifestDigest } from "./feature-partition.mjs";
import { openClassBHistory } from "./class-a-cache-history.mjs";

const HOST = "x86_64-pc-windows-msvc"; const LINUX = "x86_64-unknown-linux-gnu"; const MAC = "aarch64-apple-darwin";
const TARGETS = [HOST, LINUX, MAC];
const targetsIn = (args) => args.flatMap((a, i) => (a === "--target" ? [args[i + 1]] : []));
const cratesIn = (args) => args.flatMap((a, i) => (a === "-p" ? [args[i + 1]] : []));
const pairKey = (a, b) => [a, b].sort().join(" + ");
const allPairs = (list) => list.flatMap((a, i) => list.slice(i + 1).map((b) => pairKey(a, b))).sort();

test("rows that differ only in their target share a key wherever each names it; another configuration does not", () => {
  const host = batchKeyParts(["clippy", "--profile", "release", "--target", HOST, "--keep-going", "-p", "a", "--no-deps", "--all-targets", "--locked"]);
  const cross = batchKeyParts(["clippy", "--profile", "release", "--keep-going", "-p", "a", "--no-deps", "--all-targets", "--target", LINUX, "--locked"]);
  assert.equal(host.at, 6); assert.equal(cross.at, 4);
  assert.equal(host.target, HOST); assert.equal(cross.target, LINUX);
  assert.deepEqual(host.keyRest, cross.keyRest);
  assert.equal(host.atKey, cross.atKey);
  const other = batchKeyParts(["clippy", "--profile", "release", "--keep-going", "-p", "a", "--all-targets", "--target", LINUX, "--locked"]);
  assert.notDeepEqual(other.keyRest, cross.keyRest);
  const none = batchKeyParts(["test", "-p", "a", "--locked"]);
  assert.equal(none.target, null); assert.equal(none.atKey, none.at); assert.deepEqual(none.keyRest, none.rest);
});

/**
 * Rows for `crates` on `targets`, one configuration. `features({ crates, targets })`: the features the stand-in cargo resolves the shared
 * dependency with for a `cargo tree` naming those crates and targets.
 */
function rig({ features, sub = "clippy", crates = ["a", "b"], targets = TARGETS }) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "rk-target-groups-"));
  fs.writeFileSync(path.join(root, "Cargo.toml"), "[workspace]\nmembers = []\n");
  const calls = [];
  const spawn = async (cmd, args) => {
    calls.push({ cmd, args: [...args] });
    if (args.includes("--version")) return { status: 0, stdout: "cargo 1.98.0 (test)\n", stderr: "" };
    if (args.includes("tree")) {
      const lines = [...cratesIn(args).map((c) => `${c} v0.1.0\t`), `dep v1.0.0\t${features({ crates: cratesIn(args), targets: targetsIn(args) })}`];
      return { status: 0, stdout: `${lines.join("\n")}\n`, stderr: "" };
    }
    const lines = cratesIn(args).map((c) => ({ reason: "compiler-artifact", package_id: `path+file:///x/${c}#${c}@0.1.0`, manifest_path: path.join(root, "Cargo.toml"), target: { name: c, kind: ["lib"] } }));
    lines.push({ reason: "build-finished", success: true });
    return { status: 0, stdout: `${lines.map((l) => JSON.stringify(l)).join("\n")}\n`, stderr: "" };
  };
  const row = (crate, target) => {
    const args = target === HOST
      ? [sub, "--profile", "release", "--target", target, "--keep-going", "-p", crate, "--locked", "--no-deps", "--all-targets", "--message-format=json"]
      : [sub, "--profile", "release", "--keep-going", "-p", crate, "--locked", "--no-deps", "--all-targets", "--target", target, "--message-format=json"];
    const parts = batchKeyParts(args);
    const key = crypto.createHash("sha256").update(JSON.stringify([parts.keyRest, parts.atKey])).digest("hex").slice(0, 16);
    // A doc row carries its own RUSTDOCFLAGS, a cross row its own compiler: neither is anything `cargo tree` reads.
    const env = { CARGO_TARGET_DIR: path.join(root, "target"), ...(sub === "doc" ? { RUSTDOCFLAGS: "-D warnings" } : {}), ...(target === HOST ? {} : { [`CC_${target.replace(/-/g, "_")}`]: `cc-for-${target}` }) };
    return { class: "B", tool: "cargo", env, id: `${sub}:ws:${crate}@${target}`, args, command: `cargo ${args.join(" ")}`, batchable: { key, at: parts.at, crate, unit: `ws:${crate}`, target } };
  };
  const scans = crates.flatMap((c) => targets.map((t) => row(c, t)));
  const plan = { schema: "rightkit.deterministic-scans/1", roots: { contentRoot: root, liveRoot: root }, scans, notes: [], deferred: [], qualifyOwed: [] };
  const store = new Map();
  const cache = { read: (k) => store.get(k) ?? null, write: (k, r) => { store.set(k, { key: k, at: new Date().toISOString(), row: r }); }, digest: (scan) => `d-${scan.id}` };
  const run = () => runScans(plan, { classes: ["B"], classAEarlier: true, jobs: 4, classBJobs: 4, spawn, cache, classBBatching: true });
  const compiles = () => calls.filter((c) => c.args[0] === sub);
  const trees = () => calls.filter((c) => c.args.includes("tree"));
  return { run, calls, compiles, trees, scans, done: () => fs.rmSync(root, { recursive: true, force: true }) };
}
const notesOf = (result) => (result.notes ?? []).filter((n) => n.startsWith(TARGET_SPLIT_NOTE)).map((n) => JSON.parse(n.slice(TARGET_SPLIT_NOTE.length)));
const allPass = (result, r) => assert.deepEqual(result.scans.map((s) => s.status), r.scans.map(() => "pass"), JSON.stringify(result.scans.filter((s) => s.status !== "pass")[0]));
const noRepeatedTree = (r) => { const seen = r.trees().map((c) => c.args.join(" ")); assert.equal(new Set(seen).size, seen.length, `a cargo tree started twice:\n${seen.join("\n")}`); };

test("targets that cargo resolves alike, the host's own included, go in ONE command naming every --target and every crate", async () => {
  const r = rig({ features: () => "std" });
  try {
    const result = await r.run();
    allPass(result, r);
    assert.equal(r.compiles().length, 1, JSON.stringify(r.compiles().map((c) => c.args)));
    assert.deepEqual(targetsIn(r.compiles()[0].args).sort(), [...TARGETS].sort());
    assert.deepEqual(cratesIn(r.compiles()[0].args).sort(), ["a", "b"]);
    assert.deepEqual(notesOf(result), []);
    noRepeatedTree(r);
  } finally { r.done(); }
});

test("targets that resolve a shared package differently stay one per command, and every pair is named with that package", async () => {
  const r = rig({ features: ({ targets }) => targets.map((t) => `only-${t.split("-")[2] ?? t}`).sort().join(",") });
  try {
    const result = await r.run();
    allPass(result, r);
    assert.equal(r.compiles().length, 3, JSON.stringify(r.compiles().map((c) => c.args)));
    assert.deepEqual(r.compiles().map((c) => targetsIn(c.args).join("+")).sort(), [...TARGETS].sort());
    for (const c of r.compiles()) assert.deepEqual(cratesIn(c.args).sort(), ["a", "b"], "each target's command still names both crates");
    const notes = notesOf(result);
    assert.deepEqual(notes.map((n) => pairKey(n.a, n.b)).sort(), allPairs(TARGETS));
    for (const n of notes) { assert.equal(n.sub, "clippy"); assert.equal(n.package, "dep v1.0.0"); assert.notDeepEqual(n.aFeatures, n.bFeatures); }
    noRepeatedTree(r);
  } finally { r.done(); }
});

test("two targets that agree share a command while the third stays alone", async () => {
  const r = rig({ features: ({ targets }) => (targets.includes(LINUX) && targets.length === 1 ? "linux-only" : targets.includes(LINUX) ? "linux-only,std" : "std") });
  try {
    const result = await r.run();
    allPass(result, r);
    assert.deepEqual(r.compiles().map((c) => targetsIn(c.args).sort().join("+")).sort(), [[HOST, MAC].sort().join("+"), LINUX].sort());
    assert.deepEqual(notesOf(result).map((n) => pairKey(n.a, n.b)).sort(), [pairKey(HOST, LINUX), pairKey(MAC, LINUX)].sort());
    noRepeatedTree(r);
  } finally { r.done(); }
});

test("when cargo resolves the command naming them all differently from one target alone, none of them shares a command", async () => {
  // Each alone resolves "std"; named together cargo adds a feature: the union is not each one's own resolution.
  const r = rig({ features: ({ targets }) => (targets.length > 1 ? "std,together" : "std") });
  try {
    const result = await r.run();
    allPass(result, r);
    assert.equal(r.compiles().length, 3);
    assert.equal(notesOf(result).length, 3);
    noRepeatedTree(r);
  } finally { r.done(); }
});

test("the targets are judged for the crates a command will name, not for the larger group they came from", async () => {
  // Crate a turns `mac` on for every target; crate b only on the Mac. Named together the two targets look alike (`mac` on both). The crate
  // partition then puts b apart (alone on Linux it resolves no feature), and b alone must NOT name both targets: Linux would get `mac`.
  const features = ({ crates, targets }) => (crates.includes("a") ? "mac" : targets.includes(MAC) ? "mac" : "");
  const r = rig({ features, targets: [LINUX, MAC] });
  try {
    const result = await r.run();
    allPass(result, r);
    const shape = r.compiles().map((c) => `${cratesIn(c.args).sort().join(",")} @ ${targetsIn(c.args).sort().join("+")}`).sort();
    assert.deepEqual(shape, [`a @ ${[LINUX, MAC].sort().join("+")}`, `b @ ${LINUX}`, `b @ ${MAC}`].sort());
    assert.deepEqual(notesOf(result).map((n) => pairKey(n.a, n.b)), [pairKey(LINUX, MAC)]);
    noRepeatedTree(r);
  } finally { r.done(); }
});

test("one crate on several targets is one command when they agree (a workspace of one crate)", async () => {
  const r = rig({ features: () => "std", crates: ["a"] });
  try {
    const result = await r.run();
    allPass(result, r);
    assert.equal(r.compiles().length, 1);
    assert.deepEqual(targetsIn(r.compiles()[0].args).sort(), [...TARGETS].sort());
    noRepeatedTree(r);
  } finally { r.done(); }
});

test("with the cache rule on: after one crate is edited, only its rows are dispatched; the other crate's rows of the same several-target command stay hits", async () => {
  // Real history and a real store on disk (the forecast is made from the history before any lookup); cargo is the stand-in.
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "rk-target-groups-admission-"));
  try {
    fs.writeFileSync(path.join(root, "Cargo.toml"), "[workspace]\nmembers = []\n");
    const calls = [];
    const spawn = async (cmd, args) => {
      calls.push({ cmd, args: [...args] });
      if (args.includes("--version")) return { status: 0, stdout: "cargo 1.98.0 (test)\n", stderr: "" };
      if (args.includes("tree")) return { status: 0, stdout: `${[...cratesIn(args).map((c) => `${c} v0.1.0\t`), "dep v1.0.0\tstd"].join("\n")}\n`, stderr: "" };
      const lines = cratesIn(args).map((c) => ({ reason: "compiler-artifact", package_id: `path+file:///x/${c}#${c}@0.1.0`, manifest_path: path.join(root, "Cargo.toml"), target: { name: c, kind: ["lib"] } }));
      lines.push({ reason: "build-finished", success: true });
      return { status: 0, stdout: `${lines.map((l) => JSON.stringify(l)).join("\n")}\n`, stderr: "" };
    };
    const targets = [LINUX, MAC];
    const row = (crate, target) => {
      const args = ["clippy", "--profile", "release", "--keep-going", "-p", crate, "--locked", "--no-deps", "--all-targets", "--target", target, "--message-format=json"];
      const parts = batchKeyParts(args);
      return { class: "B", root: "content", cwd: ".", tool: "cargo", env: { [`CC_${target.replace(/-/g, "_")}`]: `cc-for-${target}` }, id: `clippy:ws:${crate}@${target}`, args, command: `cargo ${args.join(" ")}`, batchable: { key: `k-${parts.atKey}`, at: parts.at, crate, unit: `ws:${crate}`, target } };
    };
    const scans = ["core", "edited"].flatMap((c) => targets.map((t) => row(c, t)));
    const inputs = new Map(scans.map(({ id }) => [id, "original"]));
    const historyOptions = { historyRoot: path.join(root, "history"), storeDir: path.join(root, "cache") };
    openClassBHistory(historyOptions);
    const disk = fileScanCache(historyOptions.storeDir);
    const cache = { ...disk, digest: (scan) => (scan.batch ? JSON.stringify({ members: scan.batch.members.map((id) => inputs.get(id)), manifests: workspaceManifestDigest(root) }) : inputs.get(scan.id)) };
    const plan = { roots: { contentRoot: root, liveRoot: root }, scans };
    const run = async () => {
      const history = openClassBHistory(historyOptions);
      const result = await runScans(plan, { classes: ["B"], classAEarlier: true, classBBatching: true, cache, spawn, classBCacheForecast: history.forecast });
      if (!result.classBCacheDecision.mismatches.length) history.record(result.classBCacheDecision);
      return result;
    };
    const compiles = () => calls.filter((c) => c.args[0] === "clippy").map((c) => `${cratesIn(c.args).sort().join(",")} @ ${targetsIn(c.args).sort().join("+")}`);

    const first = await run();
    assert.equal(first.ok, true, JSON.stringify(first.classBCacheDecision?.mismatches ?? first.scans.filter((s) => s.status !== "pass")).slice(0, 600));
    assert.deepEqual(compiles(), [`core,edited @ ${[...targets].sort().join("+")}`]);
    assert.equal(first.classBCacheDecision.dispatchedRows.length, 4);

    const warm = await run();
    assert.equal(warm.ok, true, JSON.stringify(warm.classBCacheDecision?.mismatches).slice(0, 600));
    assert.deepEqual(warm.classBCacheDecision.dispatchedRows, []);
    assert.equal(compiles().length, 1);

    for (const t of targets) inputs.set(`clippy:ws:edited@${t}`, "edited");
    const edited = await run();
    assert.equal(edited.ok, true, JSON.stringify(edited.classBCacheDecision?.mismatches).slice(0, 600));
    assert.deepEqual([...edited.classBCacheDecision.dispatchedRows].sort(), targets.map((t) => `clippy:ws:edited@${t}`).sort(), "only the edited crate's rows are dispatched");
    assert.equal(compiles()[1], `edited @ ${[...targets].sort().join("+")}`, "the command names the edited crate only, on both targets");
    for (const t of targets) assert.equal(edited.scans.find(({ id }) => id === `clippy:ws:core@${t}`).status, "pass");

    const again = await run();
    assert.equal(again.ok, true, JSON.stringify(again.classBCacheDecision?.mismatches).slice(0, 600));
    assert.deepEqual(again.classBCacheDecision.dispatchedRows, []);
    assert.equal(compiles().length, 2);
  } finally { fs.rmSync(root, { recursive: true, force: true }); }
});

test("a subcommand the broker takes one --target for (doc) is never given several", async () => {
  const r = rig({ features: () => "std", sub: "doc" });
  try {
    await r.run();
    assert.ok(r.compiles().length >= 3, JSON.stringify(r.compiles().map((c) => c.args)));
    for (const c of r.compiles()) assert.equal(targetsIn(c.args).length, 1);
    noRepeatedTree(r);
  } finally { r.done(); }
});
