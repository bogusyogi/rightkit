// NN42: several targets share one command exactly where Cargo resolves every package they share alike (feature-partition.mjs).
import assert from "node:assert/strict";
import test from "node:test";
import { TARGET_SPLIT_NOTE, compatibleTargetClasses, groupTargetDifference, parseTreeFeatures, targetDifference, targetSplitNote, treeArgs, treeArgsForTargets } from "./feature-partition.mjs";

const tree = (rows) => parseTreeFeatures(rows.map(([p, f]) => `${p}\t${f}`).join("\n"));
const WIN = "x86_64-pc-windows-msvc"; const LINUX = "x86_64-unknown-linux-gnu"; const MAC = "aarch64-apple-darwin";

test("the tree command for several targets names each one, where the single-target command names its own", () => {
  const rest = ["clippy", "--profile", "release", "--keep-going", "--no-deps", "--all-targets"];
  const one = treeArgs({ rest, packages: ["a", "b"], target: WIN });
  assert.deepEqual(treeArgsForTargets({ rest, packages: ["a", "b"], targets: [WIN] }), one);
  const three = treeArgsForTargets({ rest, packages: ["a", "b"], targets: [WIN, LINUX, MAC] });
  assert.deepEqual(three.filter((x, i) => three[i - 1] === "--target"), [WIN, LINUX, MAC]);
  assert.deepEqual(three.filter((x, i) => x !== "--target" && three[i - 1] !== "--target"), one.filter((x, i) => x !== "--target" && one[i - 1] !== "--target"));
  assert.throws(() => treeArgsForTargets({ rest, packages: ["a"], targets: [] }), TypeError);
});

test("two targets differ when a package both build resolves with other features; a package only one builds is no difference", () => {
  const win = tree([["app v1.0.0", "default"], ["smallvec v1.16.4", "const_generics"], ["windows-sys v0.61.2", "Win32"]]);
  const mac = tree([["app v1.0.0", "default"], ["smallvec v1.16.4", "const_generics"], ["objc2 v0.6.0", "std"]]);
  const linux = tree([["app v1.0.0", "default"], ["smallvec v1.16.4", "const_generics"], ["smallvec v1.16.4", "const_generics,const_new,union"]]);
  assert.equal(targetDifference(win, mac), null);
  assert.deepEqual(targetDifference(win, linux), { package: "smallvec v1.16.4", aFeatures: ["const_generics"], bFeatures: ["const_generics", "const_generics,const_new,union"] });
});

test("targets that agree form one group; each pair kept apart is named with the package that separates it", () => {
  const same = tree([["rk-core v0.1.0", ""], ["serde v1.0.0", "derive,std"]]);
  const all = compatibleTargetClasses(new Map([[WIN, same], [LINUX, same], [MAC, same]]));
  assert.deepEqual(all, { classes: [[WIN, LINUX, MAC]], differences: [] });

  // The witness fixture's Tauri app, as cargo tree reported it on 2026-11-19: every pair differs somewhere.
  const win = tree([["bitflags v2.13.2", ""], ["bitflags v2.13.2", "serde,serde_core"], ["smallvec v1.16.4", "const_generics"], ["windows-sys v0.61.2", "Win32,Win32_Foundation,Win32_System_Com"]]);
  const linux = tree([["bitflags v2.13.2", ""], ["bitflags v2.13.2", "serde,serde_core"], ["smallvec v1.16.4", "const_generics"], ["smallvec v1.16.4", "const_generics,const_new,union"], ["windows-sys v0.61.2", "Win32,Win32_Foundation"]]);
  const mac = tree([["bitflags v2.13.2", ""], ["bitflags v2.13.2", "serde,serde_core,std"], ["smallvec v1.16.4", "const_generics"], ["windows-sys v0.61.2", "Win32,Win32_Foundation"]]);
  const split = compatibleTargetClasses(new Map([[WIN, win], [LINUX, linux], [MAC, mac]]));
  assert.deepEqual(split.classes, [[WIN], [LINUX], [MAC]]);
  assert.deepEqual(split.differences.map((d) => [d.a, d.b].sort().join(" + ")).sort(), [[WIN, LINUX], [WIN, MAC], [LINUX, MAC]].map((p) => p.sort().join(" + ")).sort());
  for (const d of split.differences) assert.notDeepEqual(d.aFeatures, d.bFeatures);

  // Two agree and the third does not: the two share a command, and both pairs with the third are named.
  const pair = compatibleTargetClasses(new Map([[WIN, same], [LINUX, tree([["rk-core v0.1.0", ""], ["serde v1.0.0", "derive"]])], [MAC, same]]));
  assert.deepEqual(pair.classes, [[WIN, MAC], [LINUX]]);
  assert.deepEqual(pair.differences.map((d) => `${d.a} + ${d.b}`).sort(), [`${WIN} + ${LINUX}`, `${MAC} + ${LINUX}`].sort());
});

test("cargo's own resolution of the command naming a whole group must be each target's resolution alone", () => {
  const win = tree([["app v1.0.0", "default"], ["dep v1.0.0", "a"]]);
  const mac = tree([["app v1.0.0", "default"], ["dep v1.0.0", "a"], ["mac-only v1.0.0", "x"]]);
  const by = new Map([[WIN, win], [MAC, mac]]);
  assert.equal(groupTargetDifference(tree([["app v1.0.0", "default"], ["dep v1.0.0", "a"], ["mac-only v1.0.0", "x"]]), by, [WIN, MAC]), null);
  assert.deepEqual(groupTargetDifference(tree([["app v1.0.0", "default"], ["dep v1.0.0", "a,b"], ["mac-only v1.0.0", "x"]]), by, [WIN, MAC]), { target: WIN, package: "dep v1.0.0", aloneFeatures: ["a"], groupFeatures: ["a,b"] });
  assert.deepEqual(groupTargetDifference(tree([["app v1.0.0", "default"], ["dep v1.0.0", "a"]]), by, [WIN, MAC]), { target: MAC, package: "mac-only v1.0.0", aloneFeatures: ["x"], groupFeatures: null });
});

test("the note is one line the contract test reads back", () => {
  const note = targetSplitNote({ sub: "clippy", cwd: "app/src-tauri", a: WIN, b: LINUX, package: "smallvec v1.16.4", aFeatures: ["const_generics"], bFeatures: ["const_generics", "const_generics,const_new,union"], extra: "dropped" });
  assert.ok(note.startsWith(TARGET_SPLIT_NOTE)); assert.ok(!note.includes("\n"));
  assert.deepEqual(JSON.parse(note.slice(TARGET_SPLIT_NOTE.length)), { sub: "clippy", cwd: "app/src-tauri", a: WIN, b: LINUX, package: "smallvec v1.16.4", aFeatures: ["const_generics"], bFeatures: ["const_generics", "const_generics,const_new,union"] });
});
