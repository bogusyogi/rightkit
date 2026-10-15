import assert from "node:assert/strict";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, symlinkSync, utimesSync, writeFileSync, lstatSync, readdirSync, realpathSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { spawnSync } from "./background-process.mjs";
const execFileSync = (cmd, args) => { const r = spawnSync(cmd, args, { encoding: "utf8" }); if (r.status !== 0) throw new Error(`${cmd} failed: ${r.stderr}`); };
import { cliFromGlobalRoots, cliFromPathBin, insideGitCheckout, resolveInstalledCli, runDiskCommand, scheduleSpec, renderSweep } from "./disk-command.mjs";
import { HOLDING_MARKER, applySweep, checkDisk, deviceOf, evaluateFloor, expandGlob, inUseByOpenPaths, literalRoot, pickHolding, planSweep, purgeHoldings, readDiskPolicy, volumeRoot } from "./disk-policy.mjs";

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
