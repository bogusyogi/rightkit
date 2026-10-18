import assert from "node:assert/strict";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, symlinkSync, utimesSync, writeFileSync, lstatSync, readdirSync, realpathSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { spawnSync } from "./background-process.mjs";
const execFileSync = (cmd, args) => { const r = spawnSync(cmd, args, { encoding: "utf8" }); if (r.status !== 0) throw new Error(`${cmd} failed: ${r.stderr}`); };
import { ensureScratchRoots, scanWorkspaceRoots, workspaceSweepAllowed, workspaceTracked } from "./disk-policy.mjs";
import { collectDependencies, evaluateBinding, installStableRoot, isUnderStable, readScheduleBinding, selfCheck, stableCliPath, stableRootPath } from "./disk-stable-root.mjs";
import { cliFromGlobalRoots, cliFromPathBin, insideGitCheckout, renderCheck, resolveInstalledCli, runDiskCommand, scheduleSpec, renderSweep } from "./disk-command.mjs";
import { HOLDING_MARKER, applySweep, checkDisk, defaultMounted, deviceOf, evaluateFloor, expandGlob, holdingBases, inUseByOpenPaths, literalRoot, moveEntry, pickHolding, planSweep, purgeHoldings, readDiskPolicy, symlinkAncestor, treeStats, volumeRoot } from "./disk-policy.mjs";

const OLD = Date.now() - 30 * 86400_000;

function fixture() {
  const root = mkdtempSync(path.join(os.tmpdir(), "rk-disk-"));
  const heavy = path.join(root, "heavy");
  const policy = {
    schema: 1, staleDays: 7,
    floors: { mac: { path: root, label: "fixture", warnFreeBytes: 1000 } },
    heavyVolume: { mac: heavy },
    holding: { mac: [{ prefix: root, path: path.join(root, "hold") }] },
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
  assert.equal(existsSync(path.join(root, "hold", "2026-10-15", "kids", "old", "f")), true);
  assert.equal(existsSync(path.join(root, "hold", HOLDING_MARKER)), true);
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
  const r = await runDiskCommand(["run", "--policy", pf], { platform: "darwin", stdout: () => {}, deps: { selfCheck: () => ({ ok: true, problems: [] }), openPaths: [], plan: { size: () => 10 }, check: { size: () => 0, space: () => ({ freeBytes: 1, totalBytes: 10 }) } } });
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
  const exists = (p) => [installed, checkoutCli].includes(p) || p === path.join("/ws/rightkit", ".git");
  assert.equal(insideGitCheckout(checkoutCli, exists), true);
  assert.equal(insideGitCheckout(installed, exists), false);
  // a pnpm store that sits inside a workspace checkout is still an installed package
  assert.equal(insideGitCheckout("/ws/.pnpm-store/v11/links/@rightkit/release/1/h/node_modules/@rightkit/release/cli/right-release.mjs", (f) => f === "/ws/.git"), false);
  assert.equal(resolveInstalledCli({ env: {}, globalRoots: () => ["/g"], exists, checkoutFile }), installed);
  assert.throws(() => resolveInstalledCli({ env: {}, globalRoots: () => ["/none"], exists, checkoutFile }), /installed @rightkit\/release/);
  assert.throws(() => resolveInstalledCli({ explicit: checkoutCli, env: {}, globalRoots: () => [], exists, checkoutFile }), /not a checkout/);
  // Running install from the installed (non-git) package is allowed, with --cli or by global-root discovery.
  assert.equal(resolveInstalledCli({ explicit: installed, env: {}, globalRoots: () => [], exists }), installed);
  assert.equal(resolveInstalledCli({ env: { RIGHT_RELEASE_INSTALLED_CLI: installed }, globalRoots: () => [], exists }), installed);
  // A real checkout directory (with .git above) is refused; an installed copy without one is accepted (real filesystem).
  const base = mkdtempSync(path.join(os.tmpdir(), "rk-cli-"));
  const mk = (dir) => { const f = path.join(dir, "cli", "right-release.mjs"); mkdirSync(path.dirname(f), { recursive: true }); writeFileSync(f, ""); return f; };
  mkdirSync(path.join(base, "checkout", ".git"), { recursive: true });
  assert.throws(() => resolveInstalledCli({ explicit: mk(path.join(base, "checkout", "packages", "release")), env: {} }), /not a checkout/);
  const copy = mk(path.join(base, "global", "node_modules", "@rightkit", "release"));
  assert.equal(resolveInstalledCli({ explicit: copy, env: {} }), copy);
});

test("holding: an item stays on its own volume; the volume root default applies without a rule", () => {
  const { root, policy } = fixture();
  file(path.join(root, "cache", "old", "f"), 10, OLD);
  const item = path.join(root, "cache", "old");
  const date = new Date("2026-10-15T12:00:00Z");
  const dir = pickHolding(item, { policy, platform: "darwin", date });
  assert.equal(dir, path.join(root, "hold", "2026-10-15"));
  assert.equal(deviceOf(dir), deviceOf(item));
  const vr = volumeRoot(path.dirname(item));
  assert.ok(item.startsWith(vr));
  assert.equal(deviceOf(vr), deviceOf(item));
  // A rule base on another device is skipped before anything is created there.
  if (process.platform !== "win32" && deviceOf("/dev") !== deviceOf(item)) {
    const elsewhere = pickHolding(item, { policy, platform: "darwin", date, bases: ["/dev/.rightkit-swept"] });
    assert.equal(elsewhere, null);
    assert.equal(existsSync("/dev/.rightkit-swept"), false);
  }
});

test("no holding dir possible on the volume: the item is deleted directly, and that is logged", () => {
  const { root, heavy, policy } = fixture();
  file(path.join(root, "cache", "old", "f"), 10, OLD);
  const plan = planSweep({ policy, platform: "darwin", openPaths: [], size: () => 10 });
  const moved = [];
  const applied = applySweep({ plan, policy, platform: "darwin", pick: () => null, move: (a, b) => moved.push([a, b]) });
  assert.equal(moved.length, 0);
  assert.equal(applied.results[0].action, "delete");
  assert.equal(applied.results[0].how, "no-holding-on-volume");
  assert.equal(existsSync(path.join(root, "cache", "old")), false);
  assert.match(readFileSync(path.join(heavy, "disk-hygiene.jsonl"), "utf8"), /no holding dir possible/);
});

test("purge: only tool-created, date-named, older-than-3-day holding dirs go", () => {
  const { root, policy } = fixture();
  const now = Date.parse("2026-10-15T12:00:00Z");
  const base = path.join(root, "hold");
  mkdirSync(base, { recursive: true });
  writeFileSync(path.join(base, HOLDING_MARKER), "");
  for (const name of ["2026-10-01", "2026-10-11", "2026-10-12", "2026-10-15", "not-a-date", "2026-10-02.tail"]) file(path.join(base, name, "f"), 10);
  const keepTarget = path.join(root, "precious");
  file(path.join(keepTarget, "f"), 10);
  symlinkSync(keepTarget, path.join(base, "2026-09-30"));
  // an unmarked sibling base with an old date dir is untouched
  const stranger = path.join(root, "stranger");
  file(path.join(stranger, "2026-01-01", "f"), 10);
  const dry = purgeHoldings({ policy, platform: "darwin", now, bases: [base, stranger], dryRun: true });
  assert.deepEqual(dry.results.map((r) => path.basename(r.path)).sort(), ["2026-10-01", "2026-10-11"]);
  assert.equal(existsSync(path.join(base, "2026-10-01")), true);
  const res = purgeHoldings({ policy, platform: "darwin", now, bases: [base, stranger] });
  assert.deepEqual(res.results.map((r) => path.basename(r.path)).sort(), ["2026-10-01", "2026-10-11"]);
  assert.equal(res.purgedBytes > 0, true);
  assert.deepEqual(readdirSync(base).sort(), [HOLDING_MARKER, "2026-09-30", "2026-10-02.tail", "2026-10-12", "2026-10-15", "not-a-date"].sort());
  assert.equal(existsSync(path.join(keepTarget, "f")), true);
  assert.equal(existsSync(path.join(stranger, "2026-01-01", "f")), true);
  assert.equal(lstatSync(path.join(base, "2026-09-30")).isSymbolicLink(), true);
});

test("run purges old holding dirs after sweeping", async () => {
  const { root, policy } = fixture();
  const pf = path.join(root, "policy.json");
  writeFileSync(pf, JSON.stringify(policy));
  const base = path.join(root, "hold");
  mkdirSync(path.join(base, "2020-01-01"), { recursive: true });
  writeFileSync(path.join(base, HOLDING_MARKER), "");
  const lines = [];
  const r = await runDiskCommand(["run", "--policy", pf], { platform: "darwin", stdout: (l) => lines.push(l), deps: { openPaths: [], plan: { size: () => 10 }, check: { size: () => 0, space: () => ({ freeBytes: 5000, totalBytes: 10000 }) }, purge: { bases: [base] } } });
  assert.equal(existsSync(path.join(base, "2020-01-01")), false);
  assert.equal(r.purged.results.length, 1);
  assert.ok(lines.some((l) => /purged 1 holding dir/.test(l)));
  const standalone = await runDiskCommand(["purge", "--dry-run", "--policy", pf], { platform: "darwin", stdout: () => {}, deps: { purge: { bases: [base] } } });
  assert.equal(standalone.exitCode, 0);
});

test("real second volume (macOS disk image): the item is renamed within its own volume", { skip: process.platform !== "darwin" }, (t) => {
  const dir = mkdtempSync(path.join(os.tmpdir(), "rk-dmg-"));
  const image = path.join(dir, "vol.dmg");
  const mount = path.join(dir, "mnt");
  mkdirSync(mount);
  try {
    execFileSync("hdiutil", ["create", "-size", "16m", "-fs", "HFS+", "-volname", "rkvol", image]);
    execFileSync("hdiutil", ["attach", image, "-mountpoint", mount, "-nobrowse", "-quiet"]);
  } catch { t.skip("cannot create a disk image here"); return; }
  try {
    const { policy } = fixture();
    policy.holding = { mac: [{ prefix: "/", path: path.join(os.homedir(), ".rightkit-swept-test-never-created") }] }; // another volume: must be skipped
    file(path.join(mount, "cache", "old", "f"), 10, OLD);
    policy.writers = [{ id: "kids", platform: "mac", path: path.join(mount, "cache"), capBytes: 1, action: "sweep", unit: "children" }];
    policy.heavyVolume.mac = path.join(dir, "heavy");
    const plan = planSweep({ policy, platform: "darwin", openPaths: [], size: () => 10 });
    assert.equal(plan.candidates.length, 1);
    const applied = applySweep({ plan, policy, platform: "darwin", date: new Date("2026-10-15T12:00:00Z") });
    assert.equal(applied.results[0].ok, true);
    assert.equal(applied.results[0].how, "rename");
    const mountReal = realpathSync(mount);
    assert.ok(realpathSync(applied.results[0].destination).startsWith(mountReal + path.sep), applied.results[0].destination);
    assert.equal(existsSync(path.join(os.homedir(), ".rightkit-swept-test-never-created")), false);
    assert.equal(purgeHoldings({ policy, platform: "darwin", now: Date.parse("2026-10-30T00:00:00Z"), bases: [path.join(mount, ".rightkit-swept")] }).results.length, 1);
    assert.equal(existsSync(path.join(mount, ".rightkit-swept", "2026-10-15")), false);
  } finally {
    try { execFileSync("hdiutil", ["detach", mount, "-force", "-quiet"]); } catch { /* ignore */ }
    rmSync(dir, { recursive: true, force: true });
  }
});

// ---- Core ML e5rt caches, over-cap cache escalation, cross-volume holding (Mac) --------------------------------------

const AGE = (days) => Date.now() - days * 86400_000;

function e5Fixture() {
  const { root, policy } = fixture();
  policy.writers = [
    { id: "cache-coreml-e5rt-tmp", platform: "mac", path: path.join(root, "Caches", "*", "com.apple.e5rt.e5bundlecache", "*", "*", "*.tmp.*"), capBytes: 1, action: "sweep", unit: "self", staleDays: 1 },
    { id: "cache-coreml-e5rt", platform: "mac", path: path.join(root, "Caches", "*", "com.apple.e5rt.e5bundlecache", "*", "*"), capBytes: 1, action: "sweep", unit: "children", staleDays: 2 },
  ];
  return { root, policy };
}

test("coreml e5rt: per-model bundles idle 2 days move, fresh ones stay, a symlinked cache is never touched", () => {
  const { root, policy } = e5Fixture();
  const e5 = (app) => path.join(root, "Caches", app, "com.apple.e5rt.e5bundlecache", "26A434", "HASH1");
  file(path.join(e5("app1"), "old.bundle", "model.bin"), 10, AGE(3));
  file(path.join(e5("app1"), "new.bundle", "model.bin"), 10);
  const elsewhere = path.join(root, "elsewhere");
  file(path.join(elsewhere, "com.apple.e5rt.e5bundlecache", "26A434", "HASH1", "old.bundle", "model.bin"), 10, AGE(30));
  symlinkSync(elsewhere, path.join(root, "Caches", "linked"));
  const plan = planSweep({ policy, platform: "darwin", home: root, openPaths: [], size: () => 10 });
  assert.deepEqual(plan.candidates.map((c) => [c.writer, path.relative(root, c.path)]), [["cache-coreml-e5rt", path.join("Caches", "app1", "com.apple.e5rt.e5bundlecache", "26A434", "HASH1", "old.bundle")]]);
  const reasons = plan.skipped.filter((s) => s.path.includes(`${path.sep}linked${path.sep}`)).map((s) => s.reason);
  assert.ok(reasons.length > 0 && reasons.every((r) => r === "symlink-ancestor"), `linked cache reasons: ${reasons}`);
  assert.equal(plan.skipped.find((s) => s.path.endsWith(`new.bundle`))?.reason, "fresh");
  assert.equal(existsSync(path.join(elsewhere, "com.apple.e5rt.e5bundlecache", "26A434", "HASH1", "old.bundle")), true);
});

test("coreml e5rt: abandoned *.tmp.* bundles go after 1 day regardless of the cap; fresh tmp stays", () => {
  const { root, policy } = e5Fixture();
  const hash = path.join(root, "Caches", "app1", "com.apple.e5rt.e5bundlecache", "26A434", "HASH1");
  file(path.join(hash, "A.tmp.11_22.bundle", "model.bin"), 10, AGE(1.5));
  file(path.join(hash, "B.tmp.33_44.bundle", "model.bin"), 10, AGE(0.5));
  file(path.join(hash, "C.bundle", "model.bin"), 10, AGE(1.5)); // not tmp: the 2-day window applies, so it stays
  const plan = planSweep({ policy, platform: "darwin", home: root, openPaths: [], size: () => 10 });
  const moved = plan.candidates.map((c) => [c.writer, path.basename(c.path)]);
  assert.deepEqual(moved, [["cache-coreml-e5rt-tmp", "A.tmp.11_22.bundle"]]);
  assert.equal(plan.skipped.find((s) => s.path.endsWith("B.tmp.33_44.bundle"))?.reason, "fresh");
  assert.equal(plan.skipped.find((s) => s.path.endsWith("C.bundle"))?.reason, "fresh");
  assert.equal(plan.skipped.filter((s) => s.path.endsWith("A.tmp.11_22.bundle")).length, 0, "moved once, not also as a 2-day child");
});

test("over-cap report cache escalates: idle children older than 7 days move only when over cap by 2x; in-use blocks", () => {
  const { root, policy } = fixture();
  const cache = path.join(root, "Caches", "heardright-engine");
  file(path.join(cache, "old", "f"), 10, AGE(10));
  file(path.join(cache, "recent", "f"), 10, AGE(3));
  policy.writers = [{ id: "cache-heard", platform: "mac", path: cache, capBytes: 100, action: "report", unit: "children", escalate: { factor: 2, staleDays: 7 } }];
  const over = planSweep({ policy, platform: "darwin", home: root, openPaths: [], size: (p) => (p === cache ? 300 : 10) });
  assert.deepEqual(over.candidates.map((c) => [c.writer, path.basename(c.path)]), [["cache-heard", "old"]]);
  assert.equal(over.skipped.find((s) => s.path.endsWith("recent"))?.reason, "fresh");
  const under = planSweep({ policy, platform: "darwin", home: root, openPaths: [], size: (p) => (p === cache ? 150 : 10) });
  assert.equal(under.candidates.length, 0, "between 1x and 2x the cap: report only");
  assert.equal(under.skipped.filter((s) => s.writer === "cache-heard").length, 0);
  const held = planSweep({ policy, platform: "darwin", home: root, openPaths: [path.join(cache, "old", "f")], size: (p) => (p === cache ? 300 : 10) });
  assert.equal(held.candidates.length, 0);
  assert.equal(held.skipped.find((s) => s.path.endsWith("old"))?.reason, "in-use");
});

test("overlapping candidates: the outer unit wins and the nested one is reported, not moved twice", () => {
  const { root, policy } = e5Fixture();
  const e5dir = path.join(root, "Caches", "app", "com.apple.e5rt.e5bundlecache");
  file(path.join(e5dir, "26A434", "HASH1", "b.bundle", "model.bin"), 10, AGE(10));
  // file() ages only the immediate parent: every directory of the outer unit must be old for the whole tree to be idle
  for (const d of [path.join(e5dir, "26A434", "HASH1"), path.join(e5dir, "26A434"), e5dir]) utimesSync(d, AGE(10) / 1000, AGE(10) / 1000);
  policy.writers = [
    { id: "whole", platform: "mac", path: e5dir, capBytes: 1, action: "sweep", unit: "self", staleDays: 7 },
    ...policy.writers.filter((w) => w.id === "cache-coreml-e5rt"),
  ];
  const plan = planSweep({ policy, platform: "darwin", home: root, openPaths: [], size: () => 10 });
  assert.deepEqual(plan.candidates.map((c) => [c.writer, c.path]), [["whole", e5dir]]);
  assert.equal(plan.skipped.find((s) => s.path.endsWith("b.bundle"))?.reason, "nested");
});

test("holding on a mounted volume: floor-volume items go to its holding dir; with no mount, the same-volume base", () => {
  const { root, policy } = fixture();
  file(path.join(root, "cache", "old", "f"), 10, OLD);
  const item = path.join(root, "cache", "old");
  const date = new Date("2026-10-15T12:00:00Z");
  policy.holding = { mac: [{ prefix: root, path: path.join(root, "dmount", ".rightkit-swept"), mount: path.join(root, "dmount") }, { prefix: root, path: path.join(root, "hold") }] };
  assert.deepEqual(holdingBases(item, { policy, platform: "darwin" })[0], { path: path.join(root, "dmount", ".rightkit-swept"), mount: path.join(root, "dmount") });
  const onMount = pickHolding(item, { policy, platform: "darwin", date, isMounted: () => true });
  assert.equal(onMount, path.join(root, "dmount", ".rightkit-swept", "2026-10-15"));
  assert.equal(existsSync(path.join(path.dirname(onMount), HOLDING_MARKER)), true, "the marker sits in the holding base, beside the date-named dirs");
  const offMount = pickHolding(item, { policy, platform: "darwin", date, isMounted: () => false });
  assert.equal(offMount, path.join(root, "hold", "2026-10-15"));
  // without an injected check, a mount path that is absent (or on the item's own volume) is not mounted
  const absent = pickHolding(item, { policy, platform: "darwin", date });
  assert.equal(absent, path.join(root, "hold", "2026-10-15"));
  assert.equal(defaultMounted(path.join(root, "nope"), deviceOf(item)), false);
  assert.equal(defaultMounted(root, deviceOf(item)), false, "same device is not another volume");
});

test("copy-verify-remove across volumes (macOS disk image): the source goes only after a verified copy", { skip: process.platform !== "darwin" }, (t) => {
  const dir = mkdtempSync(path.join(os.tmpdir(), "rk-xvol-"));
  const image = path.join(dir, "vol.dmg");
  const mount = path.join(dir, "mnt");
  mkdirSync(mount);
  try {
    execFileSync("hdiutil", ["create", "-size", "16m", "-fs", "HFS+", "-volname", "rkxvol", image]);
    execFileSync("hdiutil", ["attach", image, "-mountpoint", mount, "-nobrowse", "-quiet"]);
  } catch { t.skip("cannot create a disk image here"); return; }
  try {
    const src = path.join(dir, "src", "old");
    file(path.join(src, "f.bin"), 4096, OLD);
    file(path.join(src, "sub", "g.bin"), 2048, OLD);
    const before = treeStats(src);
    const dst = path.join(mount, "moved", "old");
    assert.equal(moveEntry(src, dst), "copy-verify-remove");
    assert.equal(existsSync(src), false);
    assert.deepEqual(treeStats(dst).entries, before.entries);
    assert.equal(readFileSync(path.join(dst, "sub", "g.bin")).length, 2048);
    // the holding rule on the mounted volume is chosen for an item on the floor volume
    const { policy } = fixture();
    policy.holding = { mac: [{ prefix: "/", path: path.join(mount, ".rightkit-swept"), mount }] };
    const item = path.join(dir, "src2");
    file(path.join(item, "f"), 10, OLD);
    const held = pickHolding(item, { policy, platform: "darwin", date: new Date("2026-10-15T12:00:00Z") });
    assert.ok(held, "a mounted volume holds the item");
    assert.ok(realpathSync(held).startsWith(realpathSync(mount) + path.sep), held);
    assert.equal(deviceOf(held) === deviceOf(item), false, "the holding dir is on the other volume");
  } finally {
    try { execFileSync("hdiutil", ["detach", mount, "-force", "-quiet"]); } catch { /* ignore */ }
    rmSync(dir, { recursive: true, force: true });
  }
});

test("symlinkAncestor: a symlinked directory between the literal root and the unit is refused", () => {
  const { root } = fixture();
  const real = path.join(root, "real");
  mkdirSync(path.join(real, "x", "y"), { recursive: true });
  symlinkSync(real, path.join(root, "link"));
  assert.equal(symlinkAncestor(path.join(root, "link", "x", "y"), root), true);
  assert.equal(symlinkAncestor(path.join(real, "x", "y"), root), false);
  assert.equal(symlinkAncestor(path.join(root, "link"), root), false, "the unit itself is judged by the caller");
});

test("readDiskPolicy rejects an escalate on a sweep writer or a non-children unit", () => {
  const { root, policy } = fixture();
  const pf = path.join(root, "bad.json");
  policy.writers = [{ id: "bad", platform: "mac", path: path.join(root, "c"), capBytes: 1, action: "sweep", unit: "children", escalate: { factor: 2, staleDays: 7 } }];
  writeFileSync(pf, JSON.stringify(policy));
  assert.throws(() => readDiskPolicy(pf), /escalate/);
  policy.writers = [{ id: "bad", platform: "mac", path: path.join(root, "c"), capBytes: 1, action: "report", unit: "self", escalate: { factor: 2, staleDays: 7 } }];
  writeFileSync(pf, JSON.stringify(policy));
  assert.throws(() => readDiskPolicy(pf), /escalate/);
});

test("committed policy: e5rt writers, over-cap escalation on the pure caches, Mac holding moves to /Volumes/D", () => {
  const { writers, holding } = readDiskPolicy();
  const mac = (id) => writers.find((w) => w.id === id && w.platform === "mac");
  assert.deepEqual([mac("cache-coreml-e5rt-tmp").action, mac("cache-coreml-e5rt-tmp").unit, mac("cache-coreml-e5rt-tmp").staleDays], ["sweep", "self", 1]);
  assert.deepEqual([mac("cache-coreml-e5rt").action, mac("cache-coreml-e5rt").unit, mac("cache-coreml-e5rt").staleDays], ["sweep", "children", 2]);
  assert.equal(mac("cache-coreml-e5rt").path, "~/Library/Caches/*/com.apple.e5rt.e5bundlecache/*/*");
  assert.ok(writers.indexOf(mac("cache-coreml-e5rt-tmp")) < writers.indexOf(mac("cache-coreml-e5rt")), "tmp writer runs first so its 1-day window wins");
  for (const id of ["cache-python", "cache-heardright-engine", "cache-cutright-engine", "cache-pnpm"]) assert.deepEqual(mac(id).escalate, { factor: 2, staleDays: 7 }, id);
  assert.deepEqual(holding.mac[0], { prefix: "/", path: "/Volumes/D/.rightkit-swept", mount: "/Volumes/D" });
});

function installedPackage(root, version, ...layout) {
  const pkgDir = path.join(root, ...layout, "node_modules", "@rightkit", "release");
  mkdirSync(path.join(pkgDir, "cli"), { recursive: true });
  writeFileSync(path.join(pkgDir, "package.json"), JSON.stringify({ name: "@rightkit/release", version }));
  const cli = path.join(pkgDir, "cli", "right-release.mjs");
  writeFileSync(cli, "");
  return cli;
}

test("pnpm 11 hashed global layout: <root>/<hash>/node_modules is found, highest version wins", () => {
  const base = mkdtempSync(path.join(os.tmpdir(), "rk-pnpm11-"));
  const root = path.join(base, "global", "v11");
  const older = installedPackage(root, "0.2.158", "aaaa1111");
  const newer = installedPackage(root, "0.2.160", "bbbb2222");
  installedPackage(root, "0.2.99", "cccc3333");
  mkdirSync(path.join(root, "dddd4444"), { recursive: true }); // a hash dir without the package
  assert.deepEqual(cliFromGlobalRoots([root]).slice(0, 2), [newer, older]);
  assert.equal(resolveInstalledCli({ env: { PATH: "" }, globalRoots: () => [root] }), newer);
  // flat npm layout still works
  const flat = installedPackage(path.join(base, "npmroot"), "0.2.50");
  assert.equal(resolveInstalledCli({ env: { PATH: "" }, globalRoots: () => [path.join(base, "npmroot", "node_modules")] }), flat);
  // a hashed copy inside a git checkout is refused
  const gitRoot = path.join(base, "co");
  mkdirSync(path.join(gitRoot, ".git"), { recursive: true });
  installedPackage(path.join(gitRoot, "global"), "9.9.9", "hash");
  assert.throws(() => resolveInstalledCli({ env: { PATH: "" }, globalRoots: () => [path.join(gitRoot, "global")] }), /not a checkout/);
});

test("the right-release bin on PATH is followed to its target mjs, before any global root", () => {
  const base = mkdtempSync(path.join(os.tmpdir(), "rk-pathbin-"));
  const root = path.join(base, "global", "v11");
  const target = installedPackage(root, "0.2.159", "h1");
  const other = installedPackage(root, "0.2.160", "h2");
  const bin = path.join(base, "bin");
  mkdirSync(bin, { recursive: true });
  // .cmd shim naming its target relative to %~dp0
  const rel = path.relative(bin, target).split(path.sep).join("\\");
  writeFileSync(path.join(bin, "right-release.cmd"), `@ECHO off\r\nnode "%~dp0\\${rel}" %*\r\n`);
  assert.deepEqual(cliFromPathBin({ env: { PATH: bin } }), [target]);
  assert.equal(resolveInstalledCli({ env: { PATH: bin }, globalRoots: () => [root] }), target);
  // sh shim with $basedir
  const sh = path.join(base, "sh");
  mkdirSync(sh);
  writeFileSync(path.join(sh, "right-release"), `#!/bin/sh\nexec node "$basedir/${path.relative(sh, other).split(path.sep).join("/")}" "$@"\n`);
  assert.deepEqual(cliFromPathBin({ env: { PATH: sh } }), [other]);
  // a symlinked bin resolves to the script itself
  const ln = path.join(base, "ln");
  mkdirSync(ln);
  symlinkSync(target, path.join(ln, "right-release"));
  assert.deepEqual(cliFromPathBin({ env: { PATH: ln } }).map((f) => realpathSync(f)), [realpathSync(target)]);
  // a bin whose target lives in a checkout is refused, falling through to the global root
  const co = path.join(base, "co");
  mkdirSync(path.join(co, ".git"), { recursive: true });
  const coCli = installedPackage(co, "0.2.200", "x");
  const cobin = path.join(base, "cobin");
  mkdirSync(cobin);
  symlinkSync(coCli, path.join(cobin, "right-release"));
  assert.equal(resolveInstalledCli({ env: { PATH: cobin }, globalRoots: () => [root] }), other);
});

test("QA evidence runs: per-writer 3-day window, tracked files never swept, --days overrides", () => {
  const { root, policy } = fixture();
  const repo = path.join(root, "ws", "app");
  mkdirSync(repo, { recursive: true });
  const git = (...args) => { const r = spawnSync("git", ["-C", repo, ...args], { encoding: "utf8" }); assert.equal(r.status, 0, r.stderr); };
  git("init", "-q");
  const DAY = 86400_000;
  const run = (name, ageDays, body = "x") => file(path.join(repo, ".cache", "rightkit-qa", name, "evidence.json"), 10, Date.now() - ageDays * DAY);
  run("run-old", 5); run("run-new", 1); run("run-tracked", 5); run("run-edge", 5);
  git("add", "-f", ".cache/rightkit-qa/run-tracked/evidence.json");
  git("-c", "user.email=a@b", "-c", "user.name=n", "commit", "-qm", "track one");
  policy.writers = [{ id: "qa", platform: "mac", path: path.join(root, "ws", "*", ".cache", "rightkit-qa", "*"), capBytes: 1, action: "sweep", unit: "self", staleDays: 3, skipTracked: true }];
  const plan = planSweep({ policy, platform: "darwin", openPaths: [], size: () => 10 });
  assert.equal(plan.staleDays, 7, "the plan reports the policy default; the writer window is its own");
  assert.deepEqual(plan.candidates.map((c) => path.basename(c.path)).sort(), ["run-edge", "run-old"]);
  const reasons = Object.fromEntries(plan.skipped.map((x) => [path.basename(x.path), x.reason]));
  assert.equal(reasons["run-tracked"], "tracked");
  assert.equal(reasons["run-new"], "fresh");
  // --days 0 applies to every writer, but a tracked run is still kept
  const all = planSweep({ policy, platform: "darwin", openPaths: [], staleDays: 0, size: () => 10 });
  assert.deepEqual(all.candidates.map((c) => path.basename(c.path)).sort(), ["run-edge", "run-new", "run-old"]);
  // applying only moves untracked stale runs; the tracked file is intact
  policy.holding = { mac: [{ prefix: root, path: path.join(root, "hold") }] };
  const applied = applySweep({ plan, policy, platform: "darwin", date: new Date("2026-10-15T12:00:00Z") });
  assert.ok(applied.results.every((r) => r.ok));
  assert.equal(existsSync(path.join(repo, ".cache", "rightkit-qa", "run-old")), false);
  assert.equal(existsSync(path.join(repo, ".cache", "rightkit-qa", "run-tracked", "evidence.json")), true);
  assert.equal(existsSync(path.join(repo, ".cache", "rightkit-qa", "run-new", "evidence.json")), true);
});

test("committed policy: QA evidence run dirs sweep at 3 days on both hosts and skip tracked files", () => {
  const { writers } = readDiskPolicy();
  const mac = writers.find((w) => w.id === "qa-evidence-runs" && w.platform === "mac");
  const win = writers.find((w) => w.id === "qa-evidence-runs" && w.platform === "win");
  for (const w of [mac, win]) assert.deepEqual([w.action, w.unit, w.staleDays, w.skipTracked], ["sweep", "self", 3, true]);
  assert.equal(mac.path, "/Volumes/D/claude/*/.cache/rightkit-qa/*");
  assert.equal(win.path, "D:\\claude\\*\\.cache\\rightkit-qa\\*");
});

// ---- allowlisted workspace roots, scratch roots ----------------------------------------------------------------------

const DAY = 86400_000;
const gitIn = (dir, ...args) => { const r = spawnSync("git", ["-C", dir, ...args], { encoding: "utf8" }); assert.equal(r.status, 0, r.stderr); };

// A workspace root that is a real git repo: tracked dir, nested checkout, allowlisted dir, and unknown dirs of various ages.
function workspaceFixture() {
  const { root: base, heavy, policy } = fixture();
  const ws = path.join(base, "ws");
  mkdirSync(ws, { recursive: true });
  gitIn(ws, "init", "-q");
  file(path.join(ws, "trk", "f"), 10, OLD);
  gitIn(ws, "add", "trk/f");
  gitIn(ws, "-c", "user.email=a@b", "-c", "user.name=n", "commit", "-qm", "track");
  file(path.join(ws, "nested", "f"), 10, OLD); mkdirSync(path.join(ws, "nested", ".git"));
  file(path.join(ws, "keep", "f"), 10, OLD);
  file(path.join(ws, "stale", "f"), 10, OLD);
  file(path.join(ws, "fresh", "f"), 10);
  // deep: every directory is old, but one file deep inside is recent
  file(path.join(ws, "deep", "a", "b", "recent"), 10);
  for (const d of ["deep/a/b", "deep/a", "deep"]) utimesSync(path.join(ws, d), OLD / 1000, OLD / 1000);
  const outside = path.join(base, "outside"); file(path.join(outside, "x"), 10, OLD);
  symlinkSync(outside, path.join(ws, "link"));
  policy.workspaceRoots = { mac: [{ path: ws }] };
  policy.workspaceAllowlist = [".git", "keep"];
  policy.unknownStaleDays = 3;
  return { base, ws, heavy, policy, outside };
}

test("workspace allowlist: only a stale, idle, unknown dir is swept", () => {
  const { ws, policy, outside } = workspaceFixture();
  file(path.join(ws, "young", "f"), 10, Date.now() - 2 * DAY); // inside the 3-day window
  const plan = planSweep({ policy, platform: "darwin", openPaths: [], size: () => 10, workspaceSweep: true });
  assert.deepEqual(plan.candidates.map((c) => c.path), [path.join(ws, "stale")]);
  const reason = Object.fromEntries(plan.skipped.map((x) => [path.basename(x.path), x.reason]));
  assert.equal(reason.fresh, "fresh");
  assert.equal(reason.young, "fresh");
  assert.equal(reason.deep, "fresh", "a recent file deep inside keeps the whole entry");
  assert.equal(reason.link, "symlink");
  for (const kept of ["trk", "nested", "keep", ".git"]) assert.equal(plan.skipped.some((x) => path.basename(x.path) === kept) || plan.candidates.some((c) => path.basename(c.path) === kept), false, `${kept} is not even considered`);
  assert.equal(existsSync(path.join(outside, "x")), true);
});

test("workspace allowlist: an open handle blocks a stale unknown dir", () => {
  const { ws, policy } = workspaceFixture();
  const plan = planSweep({ policy, platform: "darwin", openPaths: [path.join(ws, "stale", "f")], size: () => 10, workspaceSweep: true });
  assert.equal(plan.candidates.length, 0);
  assert.equal(plan.skipped.find((x) => path.basename(x.path) === "stale").reason, "in-use");
});

test("workspace allowlist: apply moves the stale entry to same-volume holding, keeps everything else, refuses a late-appearing checkout", () => {
  const { ws, policy, base } = workspaceFixture();
  const plan = planSweep({ policy, platform: "darwin", openPaths: [], size: () => 10, workspaceSweep: true });
  const applied = applySweep({ plan, policy, platform: "darwin", date: new Date("2026-10-15T12:00:00Z") });
  assert.ok(applied.results.every((r) => r.ok));
  assert.equal(existsSync(path.join(ws, "stale")), false);
  assert.equal(existsSync(path.join(base, "hold", "2026-10-15", "workspace-unknown", "stale", "f")), true);
  for (const kept of ["trk", "nested", "keep", "fresh", "deep", "link"]) assert.equal(existsSync(path.join(ws, kept)), true, kept);
  // a nested checkout that appears between plan and apply is refused
  file(path.join(ws, "late", "f"), 10, OLD);
  const plan2 = planSweep({ policy, platform: "darwin", openPaths: [], size: () => 10, workspaceSweep: true });
  mkdirSync(path.join(ws, "late", ".git"));
  const refused = applySweep({ plan: plan2, policy, platform: "darwin" });
  assert.equal(refused.results[0].ok, false);
  assert.match(refused.results[0].error, /nested git checkout/);
  assert.equal(existsSync(path.join(ws, "late", "f")), true);
});

test("workspace root that is not a repository root is never swept and check says so", () => {
  const { base, policy } = workspaceFixture();
  const plain = path.join(base, "plain");
  file(path.join(plain, "stale", "f"), 10, OLD);
  policy.workspaceRoots = { mac: [{ path: plain }] };
  assert.equal(workspaceTracked(plain, { platform: "darwin" }), null);
  const plan = planSweep({ policy, platform: "darwin", openPaths: [], size: () => 10, workspaceSweep: true });
  assert.equal(plan.candidates.length, 0);
  assert.equal(plan.skipped[0].reason, "git-unavailable");
  const check = checkDisk({ policy, platform: "darwin", size: () => 0, space: () => ({ freeBytes: 5000, totalBytes: 10000 }) });
  assert.equal(check.warn, true);
  assert.match(check.warnings[0], /cannot be read from git/);
});

test("check warns on every unknown top-level entry with size and age, not on known ones", () => {
  const { ws, policy } = workspaceFixture();
  const check = checkDisk({ policy, platform: "darwin", now: Date.now(), size: () => 2048, space: () => ({ freeBytes: 5000, totalBytes: 10000 }) });
  const names = check.unknown.map((u) => path.basename(u.path)).sort();
  assert.deepEqual(names, ["deep", "fresh", "link", "stale"]);
  assert.equal(check.unknown.find((u) => u.name === "stale").ageDays >= 29, true);
  assert.equal(check.unknown.find((u) => u.name === "stale").bytes, 2048);
  assert.equal(check.warnings.some((w) => w.includes(path.join(ws, "stale")) && /2 KB, \d+d old/.test(w)), true);
  assert.equal(check.warn, true);
  assert.match(renderCheck(check), /Unknown top-level entries \(4\)/);
  const scan = scanWorkspaceRoots({ policy, platform: "darwin" });
  assert.equal(scan.unknown.find((u) => u.name === "link").symlink, true);
});

test("scratch root: every stale idle child moves to holding, fresh and symlink children stay, missing root is created", () => {
  const { root, policy } = fixture();
  const scratch = path.join(root, "scratch");
  policy.scratchRoots = { mac: [scratch] };
  assert.deepEqual(ensureScratchRoots({ policy, platform: "darwin" }), [scratch]);
  assert.equal(existsSync(scratch), true);
  file(path.join(scratch, "old-clone", ".git", "HEAD"), 5, OLD); utimesSync(path.join(scratch, "old-clone", ".git"), OLD / 1000, OLD / 1000); utimesSync(path.join(scratch, "old-clone"), OLD / 1000, OLD / 1000);
  file(path.join(scratch, "old-file.txt"), 5, OLD);
  file(path.join(scratch, "new"), 5);
  symlinkSync(path.join(root, "cache"), path.join(scratch, "ln"));
  file(path.join(scratch, "busy", "f"), 5, OLD);
  const plan = planSweep({ policy, platform: "darwin", openPaths: [path.join(scratch, "busy", "f")], size: () => 5 });
  assert.deepEqual(plan.candidates.map((c) => path.basename(c.path)).sort(), ["old-clone", "old-file.txt"]);
  const reason = Object.fromEntries(plan.skipped.map((x) => [path.basename(x.path), x.reason]));
  assert.deepEqual([reason.new, reason.ln, reason.busy], ["fresh", "symlink", "in-use"]);
  const applied = applySweep({ plan, policy, platform: "darwin", date: new Date("2026-10-15T12:00:00Z") });
  assert.ok(applied.results.every((r) => r.ok));
  assert.equal(existsSync(path.join(root, "hold", "2026-10-15", "scratch-root", "old-file.txt")), true);
  assert.equal(existsSync(path.join(scratch, "new")), true);
});

test("workspace-unknown is report-only without the host opt-in, and CLAUDE.md or *.md or project dirs are never unknown", () => {
  const { ws, policy } = workspaceFixture();
  file(path.join(ws, "CLAUDE.md"), 10, OLD); // untracked instructions entry point
  file(path.join(ws, "notes.MD"), 10, OLD);
  for (const n of [".agent", ".membrane", "cockpit", "contributions", ".claude", ".codex", ".agents"]) file(path.join(ws, n, "f"), 10, OLD);
  file(path.join(ws, "proj-node", "package.json"), 10, OLD);
  file(path.join(ws, "proj-rust", "Cargo.toml"), 10, OLD);
  const off = planSweep({ policy, platform: "darwin", openPaths: [], size: () => 10, workspaceSweep: false });
  assert.equal(off.candidates.length, 0, "nothing moves without the opt-in");
  assert.equal(off.skipped.find((x) => path.basename(x.path) === "stale").reason, "report-only");
  const on = planSweep({ policy, platform: "darwin", openPaths: [], size: () => 10, workspaceSweep: true });
  assert.deepEqual(on.candidates.map((c) => path.basename(c.path)), ["stale"]);
  const known = scanWorkspaceRoots({ policy, platform: "darwin" }).unknown.map((u) => u.name);
  for (const n of ["CLAUDE.md", "notes.MD", ".agent", ".membrane", "cockpit", "contributions", ".claude", ".codex", ".agents", "proj-node", "proj-rust"]) assert.equal(known.includes(n), false, n);
  const applied = applySweep({ plan: on, policy, platform: "darwin", date: new Date("2026-10-16T12:00:00Z") });
  assert.ok(applied.results.every((r) => r.ok));
  assert.equal(existsSync(path.join(ws, "CLAUDE.md")), true);
});

test("the host-local opt-in flag turns the workspace sweep on and --off revokes it", async () => {
  const home = mkdtempSync(path.join(os.tmpdir(), "rk-flag-"));
  const opts = { platform: "darwin", home };
  assert.equal(workspaceSweepAllowed(opts), false);
  const lines = [];
  await runDiskCommand(["allow-workspace-sweep"], { platform: "darwin", stdout: (l) => lines.push(l), env: {}, deps: { flagHome: home } });
  assert.equal(workspaceSweepAllowed(opts), true);
  await runDiskCommand(["allow-workspace-sweep", "--off"], { platform: "darwin", stdout: (l) => lines.push(l), env: {}, deps: { flagHome: home } });
  assert.equal(workspaceSweepAllowed(opts), false);
});

test("sweep --dry-run --all lists every item, the default keeps the top 5", () => {
  const items = Array.from({ length: 8 }, (_, i) => ({ writer: "w", path: `/p/${i}`, bytes: 100 - i }));
  const plan = { candidates: items, skipped: [], totalBytes: 1, staleDays: 7 };
  assert.match(renderSweep(plan), /3 more/);
  const all = renderSweep(plan, undefined, { all: true });
  assert.doesNotMatch(all, /more/);
  assert.equal((all.match(/\/p\//g) ?? []).length, 8);
});

test("committed policy: workspace allowlist, scratch roots and the 3-day window", () => {
  const policy = readDiskPolicy();
  assert.equal(policy.unknownStaleDays, 3);
  assert.deepEqual(policy.workspaceRoots.mac.map((r) => r.path), ["/Volumes/D/claude"]);
  assert.deepEqual(policy.workspaceRoots.win.map((r) => r.path), ["D:\\claude"]);
  assert.deepEqual(policy.scratchRoots, { mac: ["/Volumes/D/scratch"], win: ["D:\\scratch"] });
  for (const name of ["tmp", "reports", "tasks", ".cache", "node_modules", ".git"]) assert.ok(policy.workspaceAllowlist.includes(name), name);
  for (const name of policy.workspaceAllowlist) assert.equal(/[\\/]/.test(name), false, `${name} is a top-level name`);
});

// ---- stable per-user root ---------------------------------------------------------------------------------------------

function fakeInstalled(base, version, { exitCode = 0 } = {}) {
  const nm = path.join(base, "pnpm", "global", "hash1", "node_modules");
  const pkg = path.join(nm, "@rightkit", "release");
  mkdirSync(path.join(pkg, "cli"), { recursive: true });
  writeFileSync(path.join(pkg, "package.json"), JSON.stringify({ name: "@rightkit/release", version, dependencies: { "smol-toml": "1.0.0" } }));
  writeFileSync(path.join(pkg, "cli", "right-release.mjs"), `console.log("${version}"); process.exit(${exitCode});\n`);
  writeFileSync(path.join(pkg, "disk-policy.json"), "{}");
  writeFileSync(path.join(pkg, "x.test.mjs"), "");
  // pnpm layout: the dependency is a symlink into a store outside node_modules
  const store = path.join(base, "store", `smol-toml-${version}`);
  mkdirSync(store, { recursive: true });
  writeFileSync(path.join(store, "package.json"), JSON.stringify({ name: "smol-toml", version: "1.0.0" }));
  writeFileSync(path.join(store, "index.js"), `// ${version}\n`);
  rmSync(path.join(nm, "smol-toml"), { force: true });
  symlinkSync(store, path.join(nm, "smol-toml"));
  return path.join(pkg, "cli", "right-release.mjs");
}

test("stable root paths follow the per-user convention on both hosts", () => {
  assert.equal(stableRootPath({ platform: "win32", env: { LOCALAPPDATA: "C:\\Users\\a\\AppData\\Local" } }), "C:\\Users\\a\\AppData\\Local\\Orthic Labs\\rightkit-release\\current");
  assert.equal(stableRootPath({ platform: "darwin", home: "/Users/a" }), "/Users/a/Library/Application Support/Orthic Labs/rightkit-release/current");
  assert.equal(stableCliPath("/s/current", "darwin"), "/s/current/node_modules/@rightkit/release/cli/right-release.mjs");
  assert.equal(isUnderStable("/s/current/node_modules/x", "/s/current", "darwin"), true);
  assert.equal(isUnderStable("/s/current-other/x", "/s/current", "darwin"), false);
  assert.equal(isUnderStable("c:\\U\\CURRENT\\a", "C:\\U\\current", "win32"), true);
});

test("install copies the package and its dependencies into the stable root; reinstall replaces current atomically", () => {
  const base = mkdtempSync(path.join(os.tmpdir(), "rk-stable-"));
  const home = path.join(base, "home");
  const first = installStableRoot({ cli: fakeInstalled(base, "1.0.0"), platform: "darwin", home });
  assert.equal(first.cli, stableCliPath(first.stable, "darwin"));
  assert.equal(existsSync(first.cli), true);
  assert.equal(first.replaced, false);
  assert.equal(lstatSync(path.join(first.stable, "node_modules", "smol-toml")).isSymbolicLink(), false, "dependency is copied, not linked into a store");
  assert.equal(existsSync(path.join(first.stable, "node_modules", "@rightkit", "release", "x.test.mjs")), false);
  assert.equal(existsSync(path.join(first.stable, "node_modules", "@rightkit", "release", "disk-policy.json")), true);
  // the source (a pnpm hash dir) vanishes on upgrade; the stable copy still runs
  rmSync(path.join(base, "pnpm"), { recursive: true }); rmSync(path.join(base, "store"), { recursive: true });
  assert.equal(spawnSync(process.execPath, [first.cli, "--version"], { encoding: "utf8" }).stdout.trim(), "1.0.0");
  // reinstall a new version from a different hash dir
  const second = installStableRoot({ cli: fakeInstalled(base, "1.1.0"), platform: "darwin", home });
  assert.equal(second.replaced, true);
  assert.equal(spawnSync(process.execPath, [second.cli, "--version"], { encoding: "utf8" }).stdout.trim(), "1.1.0");
  assert.match(readFileSync(path.join(second.stable, "node_modules", "smol-toml", "index.js"), "utf8"), /1\.1\.0/);
  assert.deepEqual(readdirSync(path.dirname(second.stable)).sort(), ["current", "current.previous"], "exactly one current.previous is kept, no staging dir");
  assert.equal(JSON.parse(readFileSync(path.join(path.dirname(second.stable), "current.previous", "install.json"), "utf8")).version, "1.0.0");
  // installing from the stable copy itself changes nothing
  assert.equal(installStableRoot({ cli: second.cli, platform: "darwin", home }).unchanged, true);
});

test("a copy that does not run at the fixed current path is rolled back from current.previous", () => {
  const base = mkdtempSync(path.join(os.tmpdir(), "rk-stable-"));
  const home = path.join(base, "home");
  const good = installStableRoot({ cli: fakeInstalled(base, "1.0.0"), platform: "darwin", home });
  assert.throws(() => installStableRoot({ cli: fakeInstalled(base, "2.0.0", { exitCode: 1 }), platform: "darwin", home }), /does not run/);
  assert.equal(spawnSync(process.execPath, [good.cli, "--version"], { encoding: "utf8" }).stdout.trim(), "1.0.0");
  assert.deepEqual(readdirSync(path.dirname(good.stable)), ["current"], "a failed smoke run at current rolls back from .previous");
  // a checkout or a foreign package is refused
  const foreign = path.join(base, "foreign", "cli", "right-release.mjs");
  mkdirSync(path.dirname(foreign), { recursive: true }); writeFileSync(foreign, ""); writeFileSync(path.join(base, "foreign", "package.json"), JSON.stringify({ name: "other" }));
  assert.throws(() => installStableRoot({ cli: foreign, platform: "darwin", home }), /not inside an @rightkit\/release package/);
});

test("a missing dependency stops the install before anything is replaced", () => {
  const base = mkdtempSync(path.join(os.tmpdir(), "rk-stable-"));
  const cli = fakeInstalled(base, "1.0.0");
  rmSync(path.join(base, "pnpm", "global", "hash1", "node_modules", "smol-toml"));
  assert.throws(() => collectDependencies(path.resolve(path.dirname(cli), "..")), /smol-toml/);
  assert.throws(() => installStableRoot({ cli, platform: "darwin", home: path.join(base, "home") }), /smol-toml/);
  assert.equal(existsSync(path.join(base, "home", "Library", "Application Support", "Orthic Labs", "rightkit-release", "current")), false);
});

// The macOS binding is read through /usr/bin/plutil (#776), so this fixture runs on macOS only.
test("the schedule binding must be the stable root; a pnpm store path or a missing target is reported, not thrown", { skip: process.platform !== "darwin" }, () => {
  const base = mkdtempSync(path.join(os.tmpdir(), "rk-bind-"));
  const home = path.join(base, "home");
  const plistFile = path.join(home, "Library", "LaunchAgents", "dev.rightkit.disk-hygiene.plist");
  const write = (cli) => { mkdirSync(path.dirname(plistFile), { recursive: true }); writeFileSync(plistFile, scheduleSpec({ platform: "darwin", home, nodePath: process.execPath, cli }).plist); };
  assert.equal(readScheduleBinding({ platform: "darwin", home }), null);
  assert.deepEqual(evaluateBinding(null, { platform: "darwin", home }), { registered: false, ok: true, problems: [] });
  write("/gone/.pnpm-store/v11/links/@rightkit/release/0.2.159/h/node_modules/@rightkit/release/cli/right-release.mjs");
  const bad = evaluateBinding(readScheduleBinding({ platform: "darwin", home }), { platform: "darwin", home });
  assert.equal(bad.ok, false);
  assert.match(bad.problems.join("\n"), /not under the stable root/);
  assert.match(bad.problems.join("\n"), /MISSING/);
  const installed = installStableRoot({ cli: fakeInstalled(base, "1.0.0"), platform: "darwin", home });
  write(installed.cli);
  const good = evaluateBinding(readScheduleBinding({ platform: "darwin", home }), { platform: "darwin", home });
  assert.deepEqual(good.problems, []);
  assert.equal(good.ok, true);
  // the stable copy is removed: missing target again, still no throw
  rmSync(installed.stable, { recursive: true });
  assert.match(evaluateBinding(readScheduleBinding({ platform: "darwin", home }), { platform: "darwin", home }).problems.join("\n"), /MISSING/);
});

test("schedule status reports a missing target clearly instead of crashing", { skip: process.platform !== "darwin" }, async () => {
  const base = mkdtempSync(path.join(os.tmpdir(), "rk-status-"));
  const oldHome = process.env.HOME;
  process.env.HOME = base;
  try {
    const plist = path.join(base, "Library", "LaunchAgents", "dev.rightkit.disk-hygiene.plist");
    mkdirSync(path.dirname(plist), { recursive: true });
    writeFileSync(plist, scheduleSpec({ platform: "darwin", home: base, nodePath: process.execPath, cli: "/gone/hash/node_modules/@rightkit/release/cli/right-release.mjs" }).plist);
    const lines = [];
    const r = await runDiskCommand(["schedule", "status"], { platform: "darwin", stdout: (l) => lines.push(l) });
    assert.equal(r.exitCode, 1);
    assert.match(lines.join("\n"), /PROBLEM: target \/gone\/hash.*MISSING/);
  } finally { process.env.HOME = oldHome; }
});

test("selfCheck flags a broken package, a missing dependency and a non-stable schedule; run exits 2 and logs it", async () => {
  const base = mkdtempSync(path.join(os.tmpdir(), "rk-self-"));
  const mod = path.join(base, "pkg");
  mkdirSync(path.join(mod, "cli"), { recursive: true });
  writeFileSync(path.join(mod, "package.json"), JSON.stringify({ name: "@rightkit/release", version: "1.0.0", dependencies: { "smol-toml": "1.0.0" } }));
  writeFileSync(path.join(mod, "cli", "right-release.mjs"), "");
  const none = () => null;
  const r1 = selfCheck({ moduleDir: mod, platform: "darwin", home: base, binding: none });
  assert.equal(r1.ok, false);
  assert.match(r1.problems.join("\n"), /smol-toml is not resolvable/);
  mkdirSync(path.join(base, "node_modules", "smol-toml"), { recursive: true });
  writeFileSync(path.join(base, "node_modules", "smol-toml", "package.json"), "{}");
  assert.equal(selfCheck({ moduleDir: mod, platform: "darwin", home: base, binding: none }).ok, true);
  const stale = selfCheck({ moduleDir: mod, platform: "darwin", home: base, binding: () => ({ node: process.execPath, cli: "/old/pnpm/hash/cli/right-release.mjs" }) });
  assert.equal(stale.ok, false);
  assert.match(stale.problems.join("\n"), /not under the stable root/);
  // run: a failing self-check is printed, logged, and fails the run after it still sweeps
  const { root, heavy, policy } = fixture();
  const pf = path.join(root, "policy.json");
  writeFileSync(pf, JSON.stringify(policy));
  const lines = [];
  const r = await runDiskCommand(["run", "--policy", pf], { platform: "darwin", stdout: (l) => lines.push(l), deps: { selfCheck: () => ({ ok: false, problems: ["binding is not stable"], version: "9" }), openPaths: [], plan: { size: () => 1 }, check: { size: () => 0, space: () => ({ freeBytes: 5000, totalBytes: 10000 }) } } });
  assert.equal(r.exitCode, 2);
  assert.match(lines.join("\n"), /ERROR: disk run self-check: binding is not stable/);
  assert.match(readFileSync(path.join(heavy, "disk-hygiene.jsonl"), "utf8"), /disk\.run\.selfcheck\.fail/);
});
