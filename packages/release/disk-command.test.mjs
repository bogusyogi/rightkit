import assert from "node:assert/strict";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, symlinkSync, utimesSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { insideGitCheckout, resolveInstalledCli, runDiskCommand, scheduleSpec, renderSweep } from "./disk-command.mjs";
import { applySweep, checkDisk, evaluateFloor, expandGlob, inUseByOpenPaths, literalRoot, planSweep, readDiskPolicy, sweptDestination } from "./disk-policy.mjs";

const OLD = Date.now() - 30 * 86400_000;

function fixture() {
  const root = mkdtempSync(path.join(os.tmpdir(), "rk-disk-"));
  const heavy = path.join(root, "heavy");
  const policy = {
    schema: 1, staleDays: 7,
    floors: { mac: { path: root, label: "fixture", warnFreeBytes: 1000 } },
    heavyVolume: { mac: heavy },
    protectedSegments: ["Application Support"],
    writers: [
      { id: "scratch", platform: "mac", path: path.join(root, "tmp", "*", "scratch"), capBytes: 100, action: "sweep", unit: "self" },
      { id: "kids", platform: "mac", path: path.join(root, "cache"), capBytes: 1000, action: "sweep", unit: "children" },
      { id: "app", platform: "mac", path: path.join(root, "Application Support"), capBytes: 10, action: "sweep", unit: "children" },
      { id: "reportonly", platform: "mac", path: path.join(root, "ro"), capBytes: 1000, action: "report" },
    ],
  };
  return { root, heavy, policy };
}

function file(p, bytes, mtimeMs = Date.now()) {
  mkdirSync(path.dirname(p), { recursive: true });
  writeFileSync(p, Buffer.alloc(bytes, 1));
  utimesSync(p, mtimeMs / 1000, mtimeMs / 1000);
  utimesSync(path.dirname(p), mtimeMs / 1000, mtimeMs / 1000);
}

test("glob expands only existing matches", () => {
  const { root } = fixture();
  file(path.join(root, "tmp", "a", "scratch", "x"), 1);
  file(path.join(root, "tmp", "b", "other", "x"), 1);
  assert.deepEqual(expandGlob(path.join(root, "tmp", "*", "scratch")), [path.join(root, "tmp", "a", "scratch")]);
  assert.deepEqual(expandGlob(path.join(root, "tmp", "zz*", "scratch")), []);
});

test("cap: writers over their cap are flagged and warn", () => {
  const { root, policy } = fixture();
  file(path.join(root, "tmp", "a", "scratch", "big"), 500);
  file(path.join(root, "ro", "small"), 5);
  const r = checkDisk({ policy, platform: "darwin", size: (p) => (p.includes("scratch") ? 500 : 5), space: () => ({ freeBytes: 5000, totalBytes: 10000 }) });
  assert.equal(r.writers.find((w) => w.id === "scratch").over, true);
  assert.equal(r.writers.find((w) => w.id === "reportonly").over, false);
  assert.equal(r.writers.find((w) => w.id === "kids").matches, 0);
  assert.equal(r.warn, true);
});

test("floor: bytes and percent thresholds, larger wins", () => {
  const floor = { label: "d", warnFreeBytes: 400, warnFreePercent: 10 };
  assert.equal(evaluateFloor(floor, { freeBytes: 365, totalBytes: 3726 }).warn, true);
  assert.equal(evaluateFloor(floor, { freeBytes: 450, totalBytes: 3726 }).thresholdBytes, 400);
  assert.equal(evaluateFloor(floor, { freeBytes: 450, totalBytes: 10000 }).warn, true); // 10% = 1000
  assert.equal(evaluateFloor(floor, { freeBytes: 1200, totalBytes: 10000 }).warn, false);
});

test("exit code is 3 on warning, 0 when clean", async () => {
  const { policy, root } = fixture();
  const pf = path.join(root, "policy.json");
  writeFileSync(pf, JSON.stringify(policy));
  const lines = [];
  const warn = await runDiskCommand(["check", "--policy", pf], { platform: "darwin", stdout: (l) => lines.push(l), deps: { check: { size: () => 0, space: () => ({ freeBytes: 1, totalBytes: 10 }) } } });
  assert.equal(warn.exitCode, 3);
  const ok = await runDiskCommand(["check", "--policy", pf, "--json"], { platform: "darwin", stdout: (l) => lines.push(l), deps: { check: { size: () => 0, space: () => ({ freeBytes: 5000, totalBytes: 10000 }) } } });
  assert.equal(ok.exitCode, 0);
});

test("stale: only old, unlocked, non-protected, non-symlink items are candidates", () => {
  const { root, policy } = fixture();
  file(path.join(root, "cache", "old", "f"), 10, OLD);
  file(path.join(root, "cache", "fresh", "f"), 10);
  file(path.join(root, "cache", "mixed", "old"), 10, OLD);
  file(path.join(root, "cache", "mixed", "new"), 10);
  file(path.join(root, "tmp", "s1", "scratch", "f"), 10, OLD);
  file(path.join(root, "Application Support", "App", "f"), 10, OLD);
  symlinkSync(path.join(root, "cache", "old"), path.join(root, "cache", "link"));
  const plan = planSweep({ policy, platform: "darwin", openPaths: [], size: () => 10 });
  const names = plan.candidates.map((c) => path.relative(root, c.path)).sort();
  assert.deepEqual(names, [path.join("cache", "old"), path.join("tmp", "s1", "scratch")]);
  const reasons = Object.fromEntries(plan.skipped.map((s) => [path.relative(root, s.path), s.reason]));
  assert.equal(reasons[path.join("cache", "fresh")], "fresh");
  assert.equal(reasons[path.join("cache", "mixed")], "fresh");
  assert.equal(reasons[path.join("cache", "link")], "symlink");
  assert.equal(reasons[path.join("Application Support", "App")], "protected");
  assert.equal(plan.candidates.some((c) => c.writer === "reportonly"), false);
});

test("in-use: an open file or cwd under the item blocks it; unknown lsof blocks too", () => {
  const { root, policy } = fixture();
  file(path.join(root, "cache", "held", "f"), 10, OLD);
  file(path.join(root, "cache", "idle", "f"), 10, OLD);
  const held = path.join(root, "cache", "held");
  assert.equal(inUseByOpenPaths(held, [path.join(held, "f")]), true);
  assert.equal(inUseByOpenPaths(held, [held + "-other/x"]), false);
  assert.equal(inUseByOpenPaths(held, undefined), true);
  const plan = planSweep({ policy, platform: "darwin", openPaths: [path.join(held, "f")], size: () => 10 });
  assert.deepEqual(plan.candidates.map((c) => path.basename(c.path)), ["idle"]);
  assert.equal(plan.skipped.find((s) => s.path === held).reason, "in-use");
  const blind = planSweep({ policy, platform: "darwin", openPaths: undefined, size: () => 10 });
  assert.equal(blind.candidates.length, 0);
});

test("sweep moves by default, logs, and never deletes without --delete", () => {
  const { root, heavy, policy } = fixture();
  file(path.join(root, "cache", "old", "f"), 10, OLD);
  const plan = planSweep({ policy, platform: "darwin", openPaths: [], size: () => 10 });
  const date = new Date("2026-10-15T12:00:00Z");
  const removed = [];
  const applied = applySweep({ plan, policy, platform: "darwin", date, remove: (p) => removed.push(p) });
  assert.equal(applied.results[0].ok, true);
  assert.equal(applied.results[0].action, "move");
  assert.equal(removed.length, 0);
  assert.equal(existsSync(path.join(root, "cache", "old")), false);
  assert.equal(existsSync(path.join(sweptDestination({ policy, platform: "darwin", date }), "kids", "old", "f")), true);
  assert.match(readFileSync(path.join(heavy, "disk-hygiene.jsonl"), "utf8"), /disk.sweep.move/);
});

test("--delete is the only path that removes, and --dry-run mutates nothing", async () => {
  const { root, policy } = fixture();
  const pf = path.join(root, "policy.json");
  writeFileSync(pf, JSON.stringify(policy));
  file(path.join(root, "cache", "old", "f"), 10, OLD);
  const deps = { openPaths: [], plan: { size: () => 10 } };
  const dry = await runDiskCommand(["sweep", "--dry-run", "--policy", pf], { platform: "darwin", stdout: () => {}, deps });
  assert.equal(dry.plan.candidates.length, 1);
  assert.equal(existsSync(path.join(root, "cache", "old")), true);
  await assert.rejects(runDiskCommand(["sweep", "--dry-run", "--delete", "--policy", pf], { platform: "darwin", stdout: () => {}, deps }));
  const del = await runDiskCommand(["sweep", "--delete", "--policy", pf], { platform: "darwin", stdout: () => {}, deps });
  assert.equal(del.applied.results[0].action, "delete");
  assert.equal(existsSync(path.join(root, "cache", "old")), false);
});

test("committed policy parses, has floors and no writer inside a protected segment", () => {
  const policy = readDiskPolicy();
  assert.equal(policy.floors.mac.warnFreeBytes, 40 * 1024 ** 3);
  assert.equal(policy.floors.win.warnFreePercent, 10);
  for (const w of policy.writers.filter((x) => x.action === "sweep")) assert.equal(policy.protectedSegments.some((s) => w.path.split(/[\\/]/).includes(s)), false, w.id);
  assert.match(renderSweep({ staleDays: 7, candidates: [], skipped: [], totalBytes: 0 }), /Dry run/);
});

test("schedule specs: launchd daily plist and hidden Windows task", () => {
  const mac = scheduleSpec({ platform: "darwin", home: "/h", nodePath: "/n", cli: "/c" });
  assert.match(mac.plist, /StartCalendarInterval/);
  assert.match(mac.plist, /<string>disk<\/string><string>run<\/string>/);
  const win = scheduleSpec({ platform: "win32", nodePath: "C:\\n.exe", cli: "C:\\c.mjs" });
  assert.match(win.script, /-Hidden/);
  assert.match(win.script, /-LogonType Interactive/);
  assert.doesNotMatch(win.script, /S4U/);
  assert.match(win.script, /C:\\c\.mjs/);
  assert.match(win.script, /-Daily/);
});

test("run: check, dry plan, move, then warns when still under the floor", async () => {
  const { root, heavy, policy } = fixture();
  const pf = path.join(root, "policy.json");
  writeFileSync(pf, JSON.stringify(policy));
  file(path.join(root, "cache", "old", "f"), 10, OLD);
  const r = await runDiskCommand(["run", "--policy", pf], { platform: "darwin", stdout: () => {}, deps: { openPaths: [], plan: { size: () => 10 }, check: { size: () => 0, space: () => ({ freeBytes: 1, totalBytes: 10 }) } } });
  assert.equal(r.exitCode, 3);
  assert.equal(existsSync(path.join(root, "cache", "old")), false);
  assert.match(readFileSync(path.join(heavy, "disk-hygiene.jsonl"), "utf8"), /disk.floor.warn/);
});

test("committed policy: Dell paths, win-temp sweep, backup leftovers, .rightkit-managed report-only", () => {
  const { writers, protectedSegments } = readDiskPolicy();
  const win = (id) => writers.find((w) => w.id === id && w.platform === "win");
  assert.equal(win("win-sccache").path, "D:\\.rightkit-managed\\rightkit-build-control\\global");
  assert.equal(win("win-shared-models").path, "${LOCALAPPDATA}\\Orthic Labs\\shared\\models");
  assert.deepEqual([win("win-temp").action, win("win-temp").unit, win("win-temp").path], ["sweep", "children", "${TEMP}"]);
  for (const w of writers.filter((x) => x.path.includes(".rightkit-managed"))) assert.equal(w.action, "report", w.id);
  assert.ok(win("win-remote-inputs"));
  for (const platform of ["win", "mac"]) {
    for (const id of ["leftover-delete-me", "leftover-node-modules-medium-rightkit", "leftover-node-modules-medium-packages", "leftover-legion-snap", "leftover-legion-final"]) {
      const w = writers.find((x) => x.id === id && x.platform === platform);
      assert.ok(w && w.action === "sweep" && w.unit === "self", `${platform} ${id}`);
    }
  }
  assert.ok(win("leftover-pulse-bak").path.endsWith("Programs\\Pulse.bak-*"));
  assert.equal(writers.some((w) => w.id === "leftover-pulse-bak" && w.platform === "mac"), false);
  assert.ok(protectedSegments.includes(".rightkit-managed"));
});

test("backup-named leftovers: only matching, stale names are swept; non-matching siblings stay", () => {
  const { root, policy } = fixture();
  policy.writers = [
    { id: "dm", platform: "mac", path: path.join(root, "ws", "*-DELETE-ME"), capBytes: 1, action: "sweep", unit: "self" },
    { id: "nm", platform: "mac", path: path.join(root, "ws", "rk", "packages", "*", "node_modules.medium-*"), capBytes: 1, action: "sweep", unit: "self" },
  ];
  file(path.join(root, "ws", "a-DELETE-ME", "f"), 10, OLD);
  file(path.join(root, "ws", "a-keep", "f"), 10, OLD);
  file(path.join(root, "ws", "new-DELETE-ME", "f"), 10);
  file(path.join(root, "ws", "rk", "packages", "p1", "node_modules.medium-1", "f"), 10, OLD);
  file(path.join(root, "ws", "rk", "packages", "p1", "node_modules", "f"), 10, OLD);
  const plan = planSweep({ policy, platform: "darwin", openPaths: [], size: () => 10 });
  assert.deepEqual(plan.candidates.map((c) => path.relative(root, c.path)).sort(), [path.join("ws", "a-DELETE-ME"), path.join("ws", "rk", "packages", "p1", "node_modules.medium-1")]);
  assert.equal(plan.skipped.find((s) => s.path.endsWith("new-DELETE-ME")).reason, "fresh");
  assert.equal(literalRoot(policy.writers[1].path), path.join(root, "ws", "rk", "packages"));
});

test("sweep never follows a symlink out of its root", () => {
  const { root, policy } = fixture();
  const outside = path.join(root, "outside");
  file(path.join(outside, "victim", "f"), 10, OLD);
  file(path.join(root, "cache", "ok", "f"), 10, OLD);
  symlinkSync(outside, path.join(root, "cache", "escape"));          // child symlink
  mkdirSync(path.join(root, "tmp"), { recursive: true });
  symlinkSync(outside, path.join(root, "tmp", "linkdir"));            // wildcard segment is a symlink: tmp/linkdir/scratch
  file(path.join(outside, "scratch", "f"), 10, OLD);
  const plan = planSweep({ policy, platform: "darwin", openPaths: [], size: () => 10 });
  assert.deepEqual(plan.candidates.map((c) => path.relative(root, c.path)), [path.join("cache", "ok")]);
  assert.equal(plan.skipped.find((s) => s.path === path.join(root, "tmp", "linkdir", "scratch")).reason, "outside-root");
  // a children root that is itself a symlink is skipped whole
  const linked = fixture();
  file(path.join(linked.root, "real", "old", "f"), 10, OLD);
  mkdirSync(path.join(linked.root, "elsewhere"), { recursive: true });
  symlinkSync(path.join(linked.root, "real"), path.join(linked.root, "cache2"));
  linked.policy.writers = [{ id: "kids2", platform: "mac", path: path.join(linked.root, "cache2"), capBytes: 1, action: "sweep", unit: "children" }];
  const p2 = planSweep({ policy: linked.policy, platform: "darwin", openPaths: [], size: () => 10 });
  assert.equal(p2.candidates.length, 0);
  assert.equal(p2.skipped[0].reason, "symlink-root");
  // apply-time guard: a planned unit swapped for a symlink is refused, target untouched
  const guard = fixture();
  file(path.join(guard.root, "cache", "old", "f"), 10, OLD);
  const gplan = planSweep({ policy: guard.policy, platform: "darwin", openPaths: [], size: () => 10 });
  const keep = path.join(guard.root, "keep");
  file(path.join(keep, "f"), 10, OLD);
  rmSync(path.join(guard.root, "cache", "old"), { recursive: true });
  symlinkSync(keep, path.join(guard.root, "cache", "old"));
  const applied = applySweep({ plan: gplan, policy: guard.policy, platform: "darwin", deleteMode: true });
  assert.equal(applied.results[0].ok, false);
  assert.equal(existsSync(path.join(keep, "f")), true);
  // a candidate not produced by a plan (no root) is refused
  const forged = applySweep({ plan: { candidates: [{ writer: "x", path: path.join(guard.root, "keep"), bytes: 1 }] }, policy: guard.policy, platform: "darwin", deleteMode: true });
  assert.equal(forged.results[0].ok, false);
  assert.equal(existsSync(path.join(keep, "f")), true);
});

test("Windows %TEMP% children sweep: only entries older than 7 days", () => {
  const { root, policy } = fixture();
  policy.floors.win = policy.floors.mac; policy.heavyVolume.win = policy.heavyVolume.mac;
  policy.writers = [{ id: "win-temp", platform: "win", path: "${TEMP}", capBytes: 1, action: "sweep", unit: "children" }];
  const temp = path.join(root, "Temp");
  file(path.join(temp, "old", "f"), 10, OLD);
  file(path.join(temp, "new", "f"), 10);
  const plan = planSweep({ policy, platform: "win32", env: { TEMP: temp }, inUse: () => false, size: () => 10 });
  assert.deepEqual(plan.candidates.map((c) => path.basename(c.path)), ["old"]);
});

test("schedule install binds the installed package, never a checkout", () => {
  const checkoutFile = "/ws/rightkit/packages/release/disk-command.mjs";
  const installed = path.join("/g", "@rightkit", "release", "cli", "right-release.mjs");
  const checkoutCli = path.join("/ws/rightkit/packages/release", "cli", "right-release.mjs");
  const exists = (p) => [installed, checkoutCli].includes(p) || p === "/ws/rightkit/.git";
  assert.equal(insideGitCheckout(checkoutCli, exists), true);
  assert.equal(insideGitCheckout(installed, exists), false);
  assert.equal(resolveInstalledCli({ env: {}, globalRoots: () => ["/g"], exists, checkoutFile }), installed);
  assert.throws(() => resolveInstalledCli({ env: {}, globalRoots: () => ["/none"], exists, checkoutFile }), /installed @rightkit\/release/);
  assert.throws(() => resolveInstalledCli({ explicit: checkoutCli, env: {}, globalRoots: () => [], exists, checkoutFile }), /not a checkout/);
});
