// rightkit#1538: (1) a cached test group carries each member's own run, so a warm run judges the members exactly as the cold run did;
// (2) a pass projected from a group onto a member's key (written before the store rule) is never read; (3) a group runs as one invocation
// only for the members whose own feature resolution the group preserves.
import assert from "node:assert/strict";
import test from "node:test";
import { meetPartitions, memberDifference, parseTreeFeatures, partitionByFeatures, partitionCacheKey, splitNote, stripSelectors, treeArgs } from "./feature-partition.mjs";
import { inheritedEnvDigest, storedPassRow, usableStoredPass } from "./deterministic-scans.mjs";

test("a row's inherited environment is keyed on what its tool reads (rightkit#1540)", () => {
  const base = { PATH: "/usr/bin", RUSTFLAGS: "-D warnings", CARGO_HOME: "/h/.cargo", PNPM_HOME: "/h/pnpm", NPM_CONFIG_CACHE: "/h/.npm", NPM_CONFIG_REGISTRY: "https://r.example", RIGHTKIT_BUILD_BROKER_SOCKET: "/tmp/a.sock", RIGHTKIT_MANAGED_AGENT_BIN: "/opt/bin", RK_SNAPSHOT_TARGETS: "x", CLAUDE_SESSION: "1" };
  const moved = { ...base, PNPM_HOME: "/other/pnpm", NPM_CONFIG_CACHE: "/other/.npm", RIGHTKIT_BUILD_BROKER_SOCKET: "/tmp/b.sock", RIGHTKIT_MANAGED_AGENT_BIN: "/other/bin", RK_SNAPSHOT_TARGETS: "y", CLAUDE_SESSION: "2" };
  // the four variables that made 103 of 119 rows miss in batch 051b, and the gate's own switches: not inputs of a cargo row
  assert.equal(inheritedEnvDigest(base, "rustright"), inheritedEnvDigest(moved, "rustright"));
  assert.equal(inheritedEnvDigest(base, "cargo"), inheritedEnvDigest(moved, "cargo"));
  // a compiler input moves: a different key
  assert.notEqual(inheritedEnvDigest(base, "rustright"), inheritedEnvDigest({ ...base, RUSTFLAGS: "-C target-cpu=native" }, "rustright"));
  // an npm row reads its registry, not pnpm's binary location or npm's cache location; it also reads the cargo family (a script can compile)
  assert.notEqual(inheritedEnvDigest(base, "pnpm"), inheritedEnvDigest({ ...base, NPM_CONFIG_REGISTRY: "https://other" }, "pnpm"));
  assert.equal(inheritedEnvDigest(base, "pnpm"), inheritedEnvDigest(moved, "pnpm"));
  assert.notEqual(inheritedEnvDigest(base, "pnpm"), inheritedEnvDigest({ ...base, RUSTFLAGS: "-C opt-level=1" }, "pnpm"));
  // a tool outside both families reads only CI
  assert.equal(inheritedEnvDigest(base, "shellcheck"), inheritedEnvDigest({ ...moved, RUSTFLAGS: "x" }, "shellcheck"));
  assert.notEqual(inheritedEnvDigest(base, "shellcheck"), inheritedEnvDigest({ ...base, CI: "1" }, "shellcheck"));
});
import { carriedScanRefusal } from "./snapshot-metadata.mjs";

const libtest = (pass, skipped = 0) => ({ runner: "libtest", tests: pass + skipped, pass, fail: 0, skipped, todo: 0 });

test("a test group's stored pass carries each member's own run and the warm hit is judged as the cold run was", () => {
  const s = { id: "test:batch:abcd1234", class: "B", command: "cargo test -p a -p b --locked", tool: "rustright", runOutside: { args: [] }, testGroup: [{ id: "test:rust:a", crate: "a" }, { id: "test:rust:b", crate: "b" }] };
  const fresh = {
    toolVersion: "cargo 1.98", results: libtest(5),
    memberRuns: {
      "test:rust:a": { status: "pass", results: libtest(3), testRun: { executables: [{ target: "a" }], outputTruncated: false, skipMarkers: 0 } },
      // b passed its executables but printed a SKIP marker: the publisher verify rejects it cold; the warm run must see the same marker.
      "test:rust:b": { status: "pass", results: libtest(2), testRun: { executables: [{ target: "b" }], outputTruncated: false, skipMarkers: 1 } },
    },
  };
  // the accepted executables (this process's paths and digests, for a serial row's reuse, rightkit#1539) are run-time state and never enter the cache
  fresh.memberRuns["test:rust:a"].acceptedBinaries = [{ package: "a", target: "a", real: "/t/out/release/deps/a-1", sha256: "x" }];
  const stored = storedPassRow(s, fresh);
  assert.equal(stored.memberRuns["test:rust:a"].acceptedBinaries, undefined, "accepted executables are stripped from the stored group row");
  delete fresh.memberRuns["test:rust:a"].acceptedBinaries;
  assert.deepEqual(stored.memberRuns, fresh.memberRuns, "the group row stores every member's own run");
  const hit = { key: "k", at: "2026-11-08T00:00:00Z", row: stored };
  assert.equal(usableStoredPass(hit, s, { cacheKey: "k" }), true);
  assert.equal(hit.row.memberRuns["test:rust:b"].testRun.skipMarkers, 1, "the skip evidence that rejected b cold is what the warm run reads");
  // The same group stored without member runs (a row written before this fix) is not a hit: the members cannot be judged.
  const { memberRuns: _m, ...legacy } = stored; void _m;
  assert.equal(usableStoredPass({ ...hit, row: legacy }, s, { cacheKey: "k" }), false);
  // A member whose own run failed inside a passing-looking group row is not a hit either.
  const bad = { ...stored, memberRuns: { ...stored.memberRuns, "test:rust:b": { ...stored.memberRuns["test:rust:b"], status: "fail", code: "EXIT" } } };
  assert.equal(usableStoredPass({ ...hit, row: bad }, s, { cacheKey: "k" }), false);
});

test("a stored row carries the run's skip and truncation evidence for a single test row too", () => {
  const s = { id: "test:rust:a", class: "B", command: "cargo test -p a --locked", tool: "rustright", runOutside: { args: [] } };
  const stored = storedPassRow(s, { toolVersion: "cargo 1.98", results: libtest(2), testRun: { executables: [], outputTruncated: false, skipMarkers: 2 } });
  assert.equal(stored.testRun.skipMarkers, 2);
  assert.equal(usableStoredPass({ key: "k", row: stored }, s, { cacheKey: "k" }), true);
});

test("a pass projected from a group onto a member's own key is never read (legacy rows, store rule)", () => {
  const s = { id: "check:rust:a", class: "B", command: "cargo check -p a --all-targets --locked", tool: "rustright" };
  const projected = { id: s.id, class: "B", command: s.command, status: "pass", batchCommand: "cargo check -p a -p b --all-targets --locked", batchedWith: { id: "check:batch:1234abcd", units: ["a", "b"] } };
  assert.equal(usableStoredPass({ key: "k", row: projected }, s, { cacheKey: "k" }), false);
  const own = { id: s.id, class: "B", command: s.command, status: "pass" };
  assert.equal(usableStoredPass({ key: "k", row: own }, s, { cacheKey: "k" }), true);
  assert.equal(usableStoredPass({ key: "other", row: own }, s, { cacheKey: "k" }), false, "a different key is no hit");
  assert.equal(usableStoredPass({ key: "k", row: { ...own, id: "check:rust:b" } }, s, { cacheKey: "k" }), false, "another row's pass is no hit");
});

test("treeArgs carries the feature flags and dev edges of the compile it stands for", () => {
  assert.deepEqual(stripSelectors(["check", "-p", "a", "--all-targets", "--target", "aarch64-apple-darwin", "--locked"]), { rest: ["check", "--all-targets", "--locked"], crates: ["a"], targets: ["aarch64-apple-darwin"] });
  assert.deepEqual(treeArgs({ rest: ["check", "--all-targets", "--locked"], packages: ["b", "a"], target: "aarch64-apple-darwin" }),
    ["tree", "--locked", "--prefix", "none", "-e", "normal,build,dev", "-f", "{p}\t{f}", "--target", "aarch64-apple-darwin", "-p", "b", "-p", "a"]);
  assert.deepEqual(treeArgs({ rest: ["doc", "--no-deps", "--all-features", "--locked"], packages: ["a"] }),
    ["tree", "--locked", "--prefix", "none", "-e", "normal,build", "-f", "{p}\t{f}", "--all-features", "-p", "a"]);
  assert.deepEqual(treeArgs({ rest: ["clippy", "--no-deps", "--all-targets", "--no-default-features", "--features", "x,y", "--locked", "--", "-D", "warnings"], packages: ["a"] }),
    ["tree", "--locked", "--prefix", "none", "-e", "normal,build,dev", "-f", "{p}\t{f}", "--no-default-features", "--features", "x,y", "-p", "a"]);
  assert.deepEqual(treeArgs({ rest: ["test", "--locked", "--message-format=json"], packages: ["a"] }).slice(4, 6), ["-e", "normal,build,dev"]);
  for (const args of [treeArgs({ rest: ["check", "--all-targets", "--locked"], packages: ["a", "b"], target: "aarch64-apple-darwin" }), treeArgs({ rest: ["doc", "--no-deps", "--all-features", "--locked"], packages: ["a"] })]) assert.equal(carriedScanRefusal(args), null, `the direct read spawn admits ${args.join(" ")}`);
  assert.match(carriedScanRefusal(["tree", "-p", "a"]), /tree.*is admitted as/);
  assert.match(carriedScanRefusal(["tree", "--locked", "--prefix", "none", "-e", "normal,build", "-f", "{p}\t{f}", "-p", "a", "--manifest-path", "/x/Cargo.toml"]), /tree.*is admitted as/);
});

test("parseTreeFeatures reads one feature list per package line, skipping banners and the (*) mark", () => {
  const text = ["rustright: request 0cce6a89", "a v0.1.0 (/ws/crates/a)\t", "libc v0.2.191\tdefault,std", "libc v0.2.191\tstd,default (*)", "serde v1.0.219\tderive,std", "serde v1.0.219\tstd", ""].join("\n");
  const m = parseTreeFeatures(text);
  assert.deepEqual([...m.get("a v0.1.0 (/ws/crates/a)")], [""]);
  assert.deepEqual([...m.get("libc v0.2.191")], ["default,std"]);
  assert.deepEqual([...m.get("serde v1.0.219")].sort(), ["derive,std", "std"]);
  assert.equal(m.has("rustright: request 0cce6a89"), false);
  assert.equal(memberDifference(parseTreeFeatures("libc v0.2.191\tstd\n"), m)?.package, "libc v0.2.191");
  assert.equal(memberDifference(parseTreeFeatures("serde v1.0.219\tstd\nserde v1.0.219\tderive,std\n"), m), null);
});

test("partitionByFeatures keeps the equivalent members together and splits the ones the group would compile differently", async () => {
  // a and c use serde with std only; b enables serde/derive, so a group with b compiles serde (and thus a's and c's graph) with derive.
  const own = {
    a: "a v1\t\nserde v1\tstd\n",
    b: "b v1\t\nserde v1\tderive,std\n",
    c: "c v1\t\nserde v1\tstd\n",
    d: "d v1\t\nlog v0.4\t\n",
  };
  const calls = [];
  const tree = async (packages) => {
    calls.push(packages.join(","));
    const lines = new Set(); const serde = packages.includes("b") ? "serde v1\tderive,std" : "serde v1\tstd";
    for (const p of packages) for (const line of own[p].trim().split("\n")) lines.add(line.startsWith("serde") ? serde : line);
    return `${[...lines].join("\n")}\n`;
  };
  const parts = await partitionByFeatures({ crates: ["a", "b", "c", "d"], tree, concurrency: 2 });
  // b's own resolution already has derive and d has no serde, so the group is their own resolution: they stay one invocation. a and c are the
  // members the group changed; among themselves they resolve serde with std only, so they are one invocation too. Nothing runs alone.
  assert.deepEqual(parts.map((p) => p.crates), [["a", "c"], ["b", "d"]]);
  assert.ok(parts.every((p) => p.split === null));
  assert.deepEqual(calls.filter((c) => c.includes(",")), ["a,b,c,d", "a,c"], "one group tree, the members' own trees, then one tree for the changed members together");
  assert.equal(calls.filter((c) => !c.includes(",")).length, 4, "each member's own tree is resolved once");
  // A group where every member is equivalent is one part, and a group where every member conflicts ends in singletons.
  assert.deepEqual((await partitionByFeatures({ crates: ["a", "c"], tree })).map((p) => p.crates), [["a", "c"]]);
  const mutual = async (packages) => (packages.length > 1 ? "x v1\t\ny v1\t\nlog v0.4\tstd,extra\n" : packages[0] === "x" ? "x v1\t\nlog v0.4\tstd\n" : "y v1\t\nlog v0.4\textra\n");
  const both = await partitionByFeatures({ crates: ["x", "y"], tree: mutual });
  assert.deepEqual(both.map((p) => p.crates), [["x"], ["y"]]);
  assert.match(splitNote(both[0].split), /x compiles alone: log v0.4 resolves to \{std\} for it and to \{extra,std\} in its group/);
  // rightkit#1538 (second review): a group naming several targets keeps every boundary any target drew. The same {a,c},{b,d} partition on two
  // targets stays {a,c},{b,d}; a target that separates c from a splits that pair everywhere; a crate alone on one target is alone with its reason.
  const two = [{ crates: ["a", "c"], split: null }, { crates: ["b", "d"], split: null }];
  assert.deepEqual(meetPartitions([two, two]).map((p) => p.crates), [["a", "c"], ["b", "d"]]);
  const cSplit = { crate: "c", package: "log v0.4", own: ["std"], group: ["extra,std"] };
  const met = meetPartitions([two, [{ crates: ["a", "b", "d"], split: null }, { crates: ["c"], split: cSplit }]]);
  assert.deepEqual(met.map((p) => p.crates), [["a"], ["b", "d"], ["c"]]);
  assert.deepEqual(met[2].split, cSplit);
  assert.equal(met[0].split, null, "a crate left alone by the meet, not by a target, carries no split reason");
  assert.deepEqual(meetPartitions([two]), two);
  assert.notEqual(partitionCacheKey({ rest: ["check"], crates: ["a", "b"], inputs: "1" }), partitionCacheKey({ rest: ["check"], crates: ["a", "b"], inputs: "2" }));
  assert.equal(partitionCacheKey({ rest: ["check"], crates: ["b", "a"], inputs: "1" }), partitionCacheKey({ rest: ["check"], crates: ["a", "b"], inputs: "1" }));
});
