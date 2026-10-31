import assert from "node:assert/strict";
import { appendFileSync, chmodSync, existsSync, lutimesSync, mkdirSync, mkdtempSync, readFileSync, rmSync, statSync, symlinkSync, utimesSync, writeFileSync, lstatSync, readdirSync, realpathSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { spawnSync } from "./background-process.mjs";
const execFileSync = (cmd, args) => { const r = spawnSync(cmd, args, { encoding: "utf8" }); if (r.status !== 0) throw new Error(`${cmd} failed: ${r.stderr}`); };
import { CACHEDIR_SIGNATURE, KEEP_MARKER, MAX_LEASE_DAYS, RUN_LEASE_FILE, createRunRoot, ensureScratchRoots, leasedByRunRoot, readRunLease, readRustrightBudget, releaseRunRoot, findStrayTargets, keptByMarker, scanWorkspaceRoots, workspaceSweepAllowed, workspaceTracked } from "./disk-policy.mjs";
import { collectDependencies, evaluateBinding, installStableRoot, isUnderStable, readScheduleBinding, selfCheck, stableCliPath, stableRootPath } from "./disk-stable-root.mjs";
import { cliFromGlobalRoots, cliFromPathBin, evaluateRunAge, fullPassDue, insideGitCheckout, renderCheck, resolveInstalledCli, runDiskCommand, scheduleSpec, renderSweep } from "./disk-command.mjs";
import { HOLDING_MARKER, applySweep, checkDisk, defaultMounted, deviceOf, evaluateFloor, expandGlob, floorList, holdingBases, holdingBasesOnVolume, lastEvent, inUseByOpenPaths, literalRoot, moveEntry, pickHolding, planSweep, purgeHoldings, readDiskPolicy, readRecentEvents, symlinkAncestor, treeStats, volumeRoot, writerCoversPath, writerTouchesPath } from "./disk-policy.mjs";

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
  // the Mac floors are a list now (internal drive + /Volumes/D), so the internal floor is read by id
  assert.equal(floorList(policy, "mac").find((f) => f.id === "mac-internal").warnFreeBytes, 40 * 1024 ** 3);
  assert.equal(policy.floors.win.warnFreePercent, 10);
  for (const w of policy.writers.filter((x) => x.action === "sweep")) assert.equal(policy.protectedSegments.some((s) => w.path.split(/[\\/]/).includes(s)), false, w.id);
  assert.match(renderSweep({ staleDays: 7, candidates: [], skipped: [], totalBytes: 0 }), /Dry run/);
});

test("schedule specs: one hourly launchd interval and one hourly hidden Windows task, both running `disk run --auto`", () => {
  const mac = scheduleSpec({ platform: "darwin", home: "/h", nodePath: "/n", cli: "/c" });
  assert.match(mac.plist, /<key>StartInterval<\/key><integer>3600<\/integer>/);
  assert.doesNotMatch(mac.plist, /StartCalendarInterval/); // both keys in one job is ambiguous in launchd: the full pass is picked by fullPassDue
  assert.match(mac.plist, /<string>disk<\/string><string>run<\/string><string>--auto<\/string>/);
  const win = scheduleSpec({ platform: "win32", nodePath: "C:\\n.exe", cli: "C:\\c.mjs" });
  assert.match(win.script, /-Hidden/);
  assert.match(win.script, /-LogonType Interactive/);
  assert.doesNotMatch(win.script, /S4U/);
  assert.match(win.script, /C:\\c\.mjs/);
  assert.doesNotMatch(win.script, /-Daily/);
  assert.equal(win.script.match(/New-ScheduledTaskTrigger/g).length, 1);
  assert.match(win.script, /New-ScheduledTaskTrigger -Once -At 00:05 -RepetitionInterval \(New-TimeSpan -Hours 1\) -RepetitionDuration \(New-TimeSpan -Days 3650\)/);
  assert.match(win.script, /-Argument '"C:\\c\.mjs" disk run --auto'/);
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
  // the sccache row follows RustRight's compiler-cache dir, which holds the 55 GB sccache part of the 250 GB build budget
  assert.equal(win("win-sccache").path, "D:\\.rightkit-managed\\rightkit-build-control\\global\\compiler-cache");
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

// Folder mtimes matter to the purge now (a dir something was moved into within the window stays), so these fixtures age every
// dated folder and its content explicitly instead of leaving the current clock on them.
test("purge: only tool-created, date-named holding dirs a full 3 days past their date go (2026-10-12 now goes too: its date + 3 days has passed)", () => {
  const { root, policy } = fixture();
  const now = Date.parse("2026-10-15T12:00:00Z");
  const base = path.join(root, "hold");
  mkdirSync(base, { recursive: true });
  writeFileSync(path.join(base, HOLDING_MARKER), "");
  for (const name of ["2026-10-01", "2026-10-11", "2026-10-12", "2026-10-15", "not-a-date", "2026-10-02.tail"]) file(path.join(base, name, "f"), 10, now - 5 * 86400_000);
  const keepTarget = path.join(root, "precious");
  file(path.join(keepTarget, "f"), 10);
  symlinkSync(keepTarget, path.join(base, "2026-09-30"));
  // an unmarked sibling base with an old date dir is untouched
  const stranger = path.join(root, "stranger");
  file(path.join(stranger, "2026-01-01", "f"), 10);
  const dry = purgeHoldings({ policy, platform: "darwin", now, bases: [base, stranger], dryRun: true });
  assert.deepEqual(dry.results.map((r) => path.basename(r.path)).sort(), ["2026-10-01", "2026-10-11", "2026-10-12"]);
  assert.equal(existsSync(path.join(base, "2026-10-01")), true);
  const res = purgeHoldings({ policy, platform: "darwin", now, bases: [base, stranger] });
  assert.deepEqual(res.results.map((r) => path.basename(r.path)).sort(), ["2026-10-01", "2026-10-11", "2026-10-12"]);
  assert.equal(res.purgedBytes > 0, true);
  assert.deepEqual(readdirSync(base).sort(), [HOLDING_MARKER, "2026-09-30", "2026-10-02.tail", "2026-10-15", "not-a-date"].sort());
  assert.equal(existsSync(path.join(keepTarget, "f")), true);
  assert.equal(existsSync(path.join(stranger, "2026-01-01", "f")), true);
  assert.equal(lstatSync(path.join(base, "2026-09-30")).isSymbolicLink(), true);
});

test("purgeHoldings removes a held tree that arrived read-only", { skip: process.platform === "win32" }, () => {
  const root = mkdtempSync(path.join(os.tmpdir(), "rk-purge-ro-"));
  const base = path.join(root, ".rightkit-swept");
  file(path.join(base, HOLDING_MARKER), 1);
  const frozen = path.join(base, "2026-10-01", "scratch-root", "frozen-source");
  file(path.join(frozen, "pkg", "a.txt"), 10);
  chmodSync(path.join(frozen, "pkg"), 0o555);
  chmodSync(frozen, 0o555);
  const res = purgeHoldings({ policy: { holdingDays: 3 }, platform: "darwin", now: Date.now() + 10 * 86400_000, bases: [base] });
  assert.deepEqual(res.results.map((r) => r.ok), [true]);
  assert.equal(existsSync(path.join(base, "2026-10-01")), false);
});

test("run purges old holding dirs after sweeping (the dir's own mtime is old too)", async () => {
  const { root, policy } = fixture();
  const now = Date.now();
  const pf = path.join(root, "policy.json");
  writeFileSync(pf, JSON.stringify(policy));
  const base = path.join(root, "hold");
  mkdirSync(path.join(base, "2020-01-01"), { recursive: true });
  utimesSync(path.join(base, "2020-01-01"), (now - 30 * DAY) / 1000, (now - 30 * DAY) / 1000);
  writeFileSync(path.join(base, HOLDING_MARKER), "");
  const lines = [];
  const r = await runDiskCommand(["run", "--policy", pf], { platform: "darwin", stdout: (l) => lines.push(l), deps: { now, selfCheck: () => ({ ok: true, problems: [] }), openPaths: [], plan: { size: () => 10 }, check: { size: () => 0, space: () => ({ freeBytes: 5000, totalBytes: 10000 }) }, purge: { bases: [base], now, days: 3 } } });
  assert.equal(r.pass, "full");
  assert.equal(existsSync(path.join(base, "2020-01-01")), false);
  assert.equal(r.purged.results.length, 1);
  assert.ok(lines.some((l) => /purged 1 holding dir/.test(l)));
  const standalone = await runDiskCommand(["purge", "--dry-run", "--policy", pf], { platform: "darwin", stdout: () => {}, deps: { purge: { bases: [base], now, days: 3 } } });
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
    // the dated folder was just written on the real clock, so "past the window" is measured from there
    assert.equal(purgeHoldings({ policy, platform: "darwin", now: Date.now() + 10 * 86400_000, bases: [path.join(mount, ".rightkit-swept")] }).results.length, 1);
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
  assert.deepEqual(readdirSync(path.dirname(second.stable)).sort(), ["current"], "no current.previous is retained after a successful reinstall (D01), and no staging dir remains");
  assert.equal(existsSync(path.join(path.dirname(second.stable), "current.previous")), false);
  assert.equal(JSON.parse(readFileSync(path.join(second.stable, "install.json"), "utf8")).version, "1.1.0");
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

// ---- floors: a list per platform, an explicit holding source, /Volumes/D ---------------------------------------------------

test("holding source is explicit: reordering or adding floors does not move the volume that may park items cross-volume", () => {
  const { root, policy } = fixture();
  file(path.join(root, "cache", "old", "f"), 10, OLD);
  const item = path.join(root, "cache", "old");
  const mount = path.join(root, "dmount");
  policy.holding = { mac: [{ prefix: root, path: path.join(mount, ".rightkit-swept"), mount }, { prefix: root, path: path.join(root, "hold") }] };
  const rule = { path: path.join(mount, ".rightkit-swept"), mount };
  // another device when the host has one (/dev), else the same device: the order check below still must hold
  const other = process.platform !== "win32" && deviceOf("/dev") !== deviceOf(root) ? "/dev" : path.join(root, "other");
  const internal = { id: "internal", path: root, label: "internal", warnFreeBytes: 1, holdingSource: true };
  const agent = { id: "d", path: other, label: "D", warnFreeBytes: 1 };
  for (const floors of [[agent, internal], [internal, agent]]) {
    policy.floors = { mac: floors };
    assert.deepEqual(holdingBases(item, { policy, platform: "darwin" })[0], rule, floors.map((f) => f.id).join(","));
  }
  // without a holdingSource flag the first floor decides (the legacy single-object form keeps working)
  policy.floors = { mac: { path: root, label: "legacy", warnFreeBytes: 1 } };
  assert.deepEqual(holdingBases(item, { policy, platform: "darwin" })[0], rule);
  if (other === "/dev") {
    policy.floors = { mac: [{ ...agent, id: "d" }] };
    assert.notDeepEqual(holdingBases(item, { policy, platform: "darwin" })[0], rule, "a floor on another volume is not the holding source");
  }
});

test("readDiskPolicy rejects a floor without a path, two holding sources and duplicate floor ids", () => {
  const { root, policy } = fixture();
  const pf = path.join(root, "bad.json");
  const attempt = (floors) => { policy.floors = { mac: floors }; writeFileSync(pf, JSON.stringify(policy)); return readDiskPolicy(pf); };
  assert.throws(() => attempt([{ id: "a", label: "a", warnFreeBytes: 1 }]), /needs a path/);
  assert.throws(() => attempt([{ id: "a", path: "/a", holdingSource: true }, { id: "b", path: "/b", holdingSource: true }]), /more than one holdingSource/);
  assert.throws(() => attempt([{ id: "a", path: "/a" }, { id: "a", path: "/b" }]), /unique/);
  assert.equal(attempt([{ id: "a", path: "/a", holdingSource: true }, { id: "b", path: "/b" }]).floors.mac.length, 2);
  assert.deepEqual(floorList({ floors: { mac: { path: "/legacy" } } }, "mac"), [{ path: "/legacy" }]);
  assert.deepEqual(floorList({ floors: {} }, "mac"), []);
});

test("check evaluates every floor and names the one that warns", () => {
  const { root, policy } = fixture();
  const dPath = path.join(root, "d");
  policy.floors = { mac: [{ id: "internal", path: root, label: "Internal", warnFreeBytes: 1000, holdingSource: true }, { id: "d", path: dPath, label: "Volume D", warnFreeBytes: 1000, hardFreeBytes: 150, agentVolume: true }] };
  const space = (p) => (p === dPath ? { freeBytes: 100, totalBytes: 10000 } : { freeBytes: 5000, totalBytes: 10000 });
  const r = checkDisk({ policy, platform: "darwin", size: () => 0, space });
  assert.equal(r.floors.length, 2);
  assert.deepEqual(r.floors.map((f) => [f.id, f.warn]), [["internal", false], ["d", true]]);
  assert.equal(r.floors[1].agentVolume, true);
  assert.equal(r.floors[1].hardWarn, true, "100 free is under the informational 150 refusal level");
  assert.equal(r.floor, r.floors[0], "result.floor stays the first floor for existing consumers");
  assert.equal(r.warnings.some((w) => w.startsWith("Volume D free")), true);
  assert.equal(r.warnings.some((w) => w.startsWith("Internal free")), false);
  assert.equal(r.warn, true);
  const text = renderCheck(r);
  assert.match(text, /^Internal: .*\n+Volume D: .*WARN.*BUILD REFUSAL LEVEL/m);
  // a hard level alone never warns
  const calm = checkDisk({ policy, platform: "darwin", size: () => 0, space: () => ({ freeBytes: 5000, totalBytes: 10000 }) });
  assert.equal(calm.warn, false);
  assert.equal(calm.floors[1].hardWarn, false);
});

test("an unmounted floor volume warns instead of throwing, and the other floors are still evaluated", () => {
  const { root, policy } = fixture();
  const dPath = path.join(root, "d");
  policy.floors = { mac: [{ id: "internal", path: root, label: "Internal", warnFreeBytes: 1000, holdingSource: true }, { id: "d", path: dPath, label: "Volume D", warnFreeBytes: 1000, agentVolume: true }] };
  const space = (p) => {
    if (p === dPath) { const e = new Error(`ENOENT: no such file or directory, statfs '${p}'`); e.code = "ENOENT"; throw e; }
    return { freeBytes: 5000, totalBytes: 10000 };
  };
  const r = checkDisk({ policy, platform: "darwin", size: () => 0, space });
  assert.equal(r.floors[0].warn, false);
  assert.deepEqual([r.floors[1].absent, r.floors[1].warn, r.floors[1].agentVolume], [true, true, true]);
  assert.match(r.floors[1].error, /ENOENT/);
  assert.equal(r.warnings.some((w) => w.includes(`floor volume ${dPath} is not mounted`)), true);
  assert.equal(r.warn, true);
  assert.match(renderCheck(r), /Volume D: free space cannot be read .*WARN/);
});

test("committed policy: Mac has a /Volumes/D floor of 250 GiB flagged agentVolume, internal is the holding source, the holding rule is unchanged", () => {
  const policy = readDiskPolicy();
  const mac = floorList(policy, "mac");
  const d = mac.find((f) => f.path === "/Volumes/D");
  assert.equal(d.warnFreeBytes, 250 * 1024 ** 3);
  assert.equal(d.agentVolume, true);
  assert.equal(d.hardFreeBytes, 200_000_000_000);
  assert.deepEqual(mac.filter((f) => f.holdingSource).map((f) => f.path), ["/System/Volumes/Data"]);
  assert.deepEqual(policy.holding.mac[0], { prefix: "/", path: "/Volumes/D/.rightkit-swept", mount: "/Volumes/D" });
  const win = floorList(policy, "win");
  assert.equal(win.length, 1);
  assert.deepEqual([win[0].id, win[0].path, win[0].agentVolume, win[0].warnFreePercent], ["win-d", "D:\\", true, 10]);
});

// ---- unmanaged remainder and uncovered top-level entries of the agent volume -------------------------------------------------

function unmanagedFixture() {
  const { root, policy } = fixture();
  const sizes = { a: 100, "a/b": 40, c: 50, zzz: 7 };
  for (const rel of ["a/b", "c", "zzz", ".Trashes", "$RECYCLE.BIN", "System Volume Information"]) mkdirSync(path.join(root, rel), { recursive: true });
  policy.floors = { mac: [{ id: "agent", path: root, label: "agent volume", warnFreeBytes: 1, agentVolume: true }] };
  policy.writers = ["a", "a/b", "c"].map((rel) => ({ id: `w-${rel.replace("/", "-")}`, platform: "mac", path: path.join(root, rel), capBytes: 1000, action: "report" }));
  const size = (p) => sizes[path.relative(root, p).split(path.sep).join("/")] ?? 1;
  return { root, policy, size, space: () => ({ freeBytes: 600, totalBytes: 1000 }) };
}

test("check: the unmanaged remainder is used minus the de-duplicated writer bytes on the agent volume", () => {
  const { root, policy, size, space } = unmanagedFixture();
  // a writer on another device is not on this volume, and a second writer on the same path is not counted twice
  const elsewhere = process.platform !== "win32" && deviceOf("/dev") !== deviceOf(root) ? [{ id: "other-volume", platform: "mac", path: "/dev", capBytes: 1 << 30, action: "report" }] : [];
  policy.writers.push({ id: "again", platform: "mac", path: path.join(root, "c"), capBytes: 1000, action: "report" }, ...elsewhere);
  const r = checkDisk({ policy, platform: "darwin", size: (p) => (p === "/dev" ? 999 : size(p)), space });
  assert.equal(r.unmanaged.length, 1);
  assert.deepEqual({ ...r.unmanaged[0], uncoveredTopLevel: undefined, writers: undefined }, { floor: "agent", usedBytes: 400, writerBytes: 150, unmanagedBytes: 250, uncoveredTopLevel: undefined, writers: undefined });
  // the outermost writers are what writerBytes is made of (the daily report's categories): the nested one and the duplicate path are not listed
  assert.deepEqual(r.unmanaged[0].writers, [{ id: "w-a", category: "w-a", bytes: 100 }, { id: "w-c", category: "w-c", bytes: 50 }]);
  // writers are unchanged by the accounting and carry a category
  assert.deepEqual(r.writers.map((w) => [w.id, w.category, w.bytes]).slice(0, 3), [["w-a", "w-a", 100], ["w-a-b", "w-a-b", 40], ["w-c", "w-c", 50]]);
});

test("check: a writer category replaces its id as the report category", () => {
  const { policy, size, space } = unmanagedFixture();
  policy.writers[0].category = "agent-cache";
  const r = checkDisk({ policy, platform: "darwin", size, space });
  assert.deepEqual(r.writers.map((w) => w.category), ["agent-cache", "w-a-b", "w-c"]);
});

test("check: a top-level entry no writer names or contains is listed and warned; covered, nested-covered and OS entries are not", () => {
  const { root, policy, size, space } = unmanagedFixture();
  // a writer deeper than the entry covers it (the entry is then an ancestor of a writer), like .rightkit-managed over its children
  mkdirSync(path.join(root, "managed", "runs"), { recursive: true });
  policy.writers.push({ id: "managed-runs", platform: "mac", path: path.join(root, "managed", "runs"), capBytes: 1000, action: "report" });
  const r = checkDisk({ policy, platform: "darwin", size, space });
  const u = r.unmanaged[0];
  assert.deepEqual(u.uncoveredTopLevel, [{ name: "zzz", path: path.join(root, "zzz"), symlink: false, bytes: 7 }]);
  assert.equal(r.warnings.some((w) => w.includes(`no writer covers ${path.join(root, "zzz")}`) && w.includes("add one to disk-policy.json")), true);
  assert.equal(r.warn, true);
  for (const covered of ["a", "c", "managed", ".Trashes", "$RECYCLE.BIN", "System Volume Information"]) assert.equal(u.uncoveredTopLevel.some((e) => e.name === covered), false, covered);
  // volumeIgnore replaces the default list
  policy.volumeIgnore = ["zzz"];
  const ignored = checkDisk({ policy, platform: "darwin", size, space });
  assert.deepEqual(ignored.unmanaged[0].uncoveredTopLevel.map((e) => e.name), ["$RECYCLE.BIN", ".Trashes", "System Volume Information"]);
});

test("check: a symlink at the volume top level is listed without being followed; a missing or unflagged floor adds nothing", () => {
  const { root, policy, size, space } = unmanagedFixture();
  symlinkSync(path.join(root, "c"), path.join(root, "link"));
  const r = checkDisk({ policy, platform: "darwin", size: (p) => { assert.notEqual(p, path.join(root, "link"), "a symlink is not sized"); return size(p); }, space });
  assert.deepEqual(r.unmanaged[0].uncoveredTopLevel.map((e) => [e.name, e.symlink, e.bytes]), [["link", true, 0], ["zzz", false, 7]]);
  policy.floors = { mac: [{ id: "agent", path: path.join(root, "not-mounted"), label: "agent volume", warnFreeBytes: 1, agentVolume: true }, { id: "plain", path: root, label: "plain", warnFreeBytes: 1 }] };
  const none = checkDisk({ policy, platform: "darwin", size, space: (p) => { if (p.endsWith("not-mounted")) throw new Error("ENOENT"); return space(); } });
  assert.deepEqual(none.unmanaged, []);
});

test("check output shows the unmanaged remainder and the uncovered top-level entries", () => {
  const { policy, size, space } = unmanagedFixture();
  const text = renderCheck(checkDisk({ policy, platform: "darwin", size, space }));
  assert.match(text, /unmanaged remainder on agent {2}\d+ KB \(estimate: used \d+ KB - writers \d+ KB\)/);
  assert.match(text, /Uncovered top-level on agent \(1\):\n +\d+ KB {2}.*zzz/);
  assert.equal(renderCheck({ floor: { label: "x", freeBytes: 2048, totalBytes: 4096, thresholdBytes: 1024 }, writers: [], warnings: [] }).split("\n").length, 1, "a result without floors, unmanaged or unknown still renders");
});

test("writerCoversPath: an ancestor or equal path covers, a sibling does not, `*` stays inside one segment", () => {
  const w = (p) => ({ id: "w", platform: "mac", path: p });
  const mac = { platform: "darwin" };
  assert.equal(writerCoversPath(w("/Volumes/D/agent-cache"), "/Volumes/D/agent-cache/tmp", mac), true, "ancestor");
  assert.equal(writerCoversPath(w("/Volumes/D/agent-cache"), "/Volumes/D/agent-cache", mac), true, "equal");
  assert.equal(writerCoversPath(w("/Volumes/D/agent-cache/tmp"), "/Volumes/D/agent-cache", mac), false, "a descendant does not cover its ancestor");
  assert.equal(writerCoversPath(w("/Volumes/D/agent-cache"), "/Volumes/D/agent-cachex/tmp", mac), false, "sibling with a shared prefix");
  assert.equal(writerCoversPath(w("/Volumes/D/*/tmp"), "/Volumes/D/a/tmp/x", mac), true);
  assert.equal(writerCoversPath(w("/Volumes/D/*/tmp"), "/Volumes/D/a/b/tmp", mac), false, "`*` is one segment");
  assert.equal(writerCoversPath(w("/Volumes/D/scratch/legion*"), "/Volumes/D/scratch/legion-1/x", mac), true);
  assert.equal(writerCoversPath(w("/Volumes/D/scratch/legion*"), "/Volumes/D/scratch/rk-1", mac), false);
  assert.equal(writerCoversPath(w("/Volumes/D/Claude"), "/Volumes/D/claude/x", mac), false, "mac is case-sensitive");
  assert.equal(writerCoversPath(w("D:\\Agent-Cache"), "d:\\agent-cache\\TMP", { platform: "win32" }), true, "windows is case-insensitive and accepts both separators");
  assert.equal(writerCoversPath(w("${TEMP}"), "/anything/at/all", mac), false, "an unset variable proves nothing");
  assert.equal(writerCoversPath(w("${TEMP}"), "/tmp/x", { ...mac, env: { TEMP: "/tmp" } }), true, "a set variable is expanded");
  assert.equal(writerCoversPath(w("~/Library/Caches/pnpm"), "/Users/a/Library/Caches/pnpm/v3", { ...mac, home: "/Users/a" }), true, "~ expands against the given home");
  assert.equal(writerCoversPath(w("~/Library/Caches/pnpm"), "/Users/b/Library/Caches/pnpm/v3", { ...mac, home: "/Users/a" }), false);
});

test("writerTouchesPath: a writer below the entry accounts for it as well", () => {
  const w = (p) => ({ id: "w", platform: "mac", path: p });
  const mac = { platform: "darwin" };
  assert.equal(writerTouchesPath(w("/Volumes/D/.rightkit-managed/runs"), "/Volumes/D/.rightkit-managed", mac), true);
  assert.equal(writerCoversPath(w("/Volumes/D/.rightkit-managed/runs"), "/Volumes/D/.rightkit-managed", mac), false);
  assert.equal(writerTouchesPath(w("/Volumes/D/*-DELETE-ME"), "/Volumes/D/x-DELETE-ME", mac), true);
  assert.equal(writerTouchesPath(w("/Volumes/D/.rightkit-managed/runs"), "/Volumes/D/other", mac), false);
});

// ---- writers for every root on the agent volume ----------------------------------------------------------------------------

test("committed policy: the agent-volume roots have writers on both hosts, .rightkit-managed rows only report, build budget caps", () => {
  const { writers, protectedSegments } = readDiskPolicy();
  const on = (platform) => writers.filter((w) => w.platform === platform);
  for (const platform of ["mac", "win"]) {
    const ids = on(platform).map((w) => w.id);
    assert.equal(new Set(ids).size, ids.length, `${platform} writer ids are unique`);
    const w = (id) => on(platform).find((x) => x.id === id);
    for (const id of ["agent-tmp", "agent-cache", "dev-cache", "agent-home", "build-home", "build-home-tmp", "managed-runs", "managed-artifacts", "remote-workspaces", "holding-volume", "scratch-total"]) assert.ok(w(id), `${platform} ${id}`);
    for (const id of ["agent-tmp", "build-home-tmp"]) assert.deepEqual([w(id).action, w(id).unit, w(id).staleDays], ["sweep", "children", 1], `${platform} ${id}`);
    assert.equal(w("agent-home").action, "report", "agent homes hold chat state: never swept");
    for (const x of on(platform).filter((y) => y.path.includes(".rightkit-managed") || y.path.includes(".rightkit-swept"))) assert.equal(x.action, "report", `${platform} ${x.id}`);
    for (const x of on(platform).filter((y) => y.action === "sweep")) assert.equal(protectedSegments.some((s) => x.path.split(/[\\/]/).includes(s)), false, `${platform} ${x.id}`);
    assert.equal(w("build-control-targets").capBytes, 250_000_000_000, `${platform} the whole RustRight build budget`);
  }
  assert.equal(on("mac").find((w) => w.id === "build-control-sccache").capBytes, 55_000_000_000);
  assert.equal(on("win").find((w) => w.id === "win-sccache").capBytes, 55_000_000_000);
  assert.equal(on("win").find((w) => w.id === "build-control-targets").path, "D:\\.rightkit-managed\\rightkit-build-control");
  // planSweep dedupes units by first writer: the 1-day rows must come before win-temp, which uses the default window
  const winTemp = writers.findIndex((w) => w.id === "win-temp");
  for (const id of ["agent-tmp", "build-home-tmp"]) for (const platform of ["mac", "win"]) assert.ok(writers.findIndex((w) => w.id === id && w.platform === platform) < winTemp, `${platform} ${id} precedes win-temp`);
});

test("agent-tmp wins over win-temp when TEMP is redirected to the same directory: idle one day moves, a younger child stays", () => {
  const { root, policy } = fixture();
  const temp = path.join(root, "agent-cache", "tmp");
  file(path.join(temp, "idle-2d", "f"), 10, AGE(2));
  file(path.join(temp, "young-12h", "f"), 10, AGE(0.5));
  const committed = readDiskPolicy().writers;
  // the committed rows (their order, window and unit), pointed at a fixture directory
  policy.writers = ["agent-tmp", "win-temp"].map((id) => ({ ...committed.find((w) => w.id === id && w.platform === "win"), ...(id === "agent-tmp" ? { path: temp } : {}) }));
  policy.floors.win = policy.floors.mac; policy.heavyVolume.win = policy.heavyVolume.mac;
  const plan = planSweep({ policy, platform: "win32", env: { TEMP: temp }, inUse: () => false, size: () => 10 });
  assert.deepEqual(plan.candidates.map((c) => [c.writer, path.basename(c.path)]), [["agent-tmp", "idle-2d"]]);
  assert.equal(plan.skipped.find((s) => s.path.endsWith("young-12h")).reason, "fresh");
  // without the committed order the 7-day default of win-temp would have kept both
  const reversed = planSweep({ policy: { ...policy, writers: [...policy.writers].reverse() }, platform: "win32", env: { TEMP: temp }, inUse: () => false, size: () => 10 });
  assert.deepEqual(reversed.candidates, []);
});

// ---- lifetimes: scratch idle 1 day, holding exactly `holdingDays` --------------------------------------------------------------

test("scratch: a child idle 25 hours moves, one idle 23 hours stays, workspace-unknown keeps its 3-day window, --days overrides both", () => {
  const { base, ws, policy } = workspaceFixture();
  const scratch = path.join(base, "scratch");
  policy.scratchRoots = { mac: [scratch] };
  policy.scratchStaleDays = 1;
  const now = Date.now();
  file(path.join(scratch, "idle-25h", "f"), 5, now - 25 * 3600_000);
  file(path.join(scratch, "idle-23h", "f"), 5, now - 23 * 3600_000);
  file(path.join(scratch, "idle-2d", "f"), 5, now - 2 * DAY);
  file(path.join(ws, "young", "f"), 10, now - 2 * DAY); // unknown workspace entry idle 2 days: inside its 3-day window
  const plan = planSweep({ policy, platform: "darwin", now, openPaths: [], size: () => 5, workspaceSweep: true });
  assert.deepEqual(plan.candidates.filter((c) => c.writer === "scratch-root").map((c) => path.basename(c.path)).sort(), ["idle-25h", "idle-2d"]);
  assert.equal(plan.skipped.find((s) => s.path.endsWith("idle-23h")).reason, "fresh");
  assert.deepEqual(plan.candidates.filter((c) => c.writer === "workspace-unknown").map((c) => path.basename(c.path)), ["stale"]);
  assert.equal(plan.skipped.find((s) => s.path === path.join(ws, "young")).reason, "fresh");
  // `--days N` is one window for everything
  const wide = planSweep({ policy, platform: "darwin", now, staleDays: 5, openPaths: [], size: () => 5, workspaceSweep: true });
  assert.deepEqual(wide.candidates.filter((c) => c.writer === "scratch-root"), []);
  // a policy without scratchStaleDays keeps the old shared window
  delete policy.scratchStaleDays;
  const legacy = planSweep({ policy, platform: "darwin", now, openPaths: [], size: () => 5, workspaceSweep: true });
  assert.deepEqual(legacy.candidates.filter((c) => c.writer === "scratch-root"), []);
});

test("purge: a dated folder goes exactly `days` after its date, not a day later", () => {
  const { root, policy } = fixture();
  const base = path.join(root, "hold");
  mkdirSync(base, { recursive: true });
  writeFileSync(path.join(base, HOLDING_MARKER), "");
  const old = Date.parse("2026-09-01T00:00:00Z");
  for (const name of ["2026-10-11", "2026-10-12", "2026-10-13", "2026-10-15"]) file(path.join(base, name, "f"), 10, old);
  const at = (iso, days = 3) => purgeHoldings({ policy, platform: "darwin", now: Date.parse(iso), days, bases: [base], dryRun: true }).results.map((r) => path.basename(r.path)).sort();
  assert.deepEqual(at("2026-10-15T12:00:00Z"), ["2026-10-11", "2026-10-12"], "10-12 + 3 days = 10-15T00:00Z has passed");
  assert.deepEqual(at("2026-10-15T00:00:00Z"), ["2026-10-11", "2026-10-12"], "exactly on the boundary");
  assert.deepEqual(at("2026-10-14T23:59:59Z"), ["2026-10-11"], "one second before it");
  assert.deepEqual(at("2026-10-16T12:00:00Z", 1), ["2026-10-11", "2026-10-12", "2026-10-13", "2026-10-15"], "1 day: every folder dated before the 16th is past its window");
  assert.deepEqual(at("2026-10-15T12:00:00Z", 1), ["2026-10-11", "2026-10-12", "2026-10-13"], "1 day: today's folder is not");
  assert.deepEqual(at("2026-10-15T12:00:00Z", 0), ["2026-10-11", "2026-10-12", "2026-10-13", "2026-10-15"], "0 days: today's folder goes too");
});

test("purge: a dated folder something was moved into within the window stays, even when its date is old", () => {
  const { root, policy } = fixture();
  const base = path.join(root, "hold");
  mkdirSync(base, { recursive: true });
  writeFileSync(path.join(base, HOLDING_MARKER), "");
  const now = Date.parse("2026-10-15T12:00:00Z");
  const names = (days = 1) => purgeHoldings({ policy, platform: "darwin", now, days, bases: [base], dryRun: true }).results.map((r) => path.basename(r.path)).sort();
  const oldDay = Date.parse("2026-09-01T00:00:00Z");
  file(path.join(base, "2026-10-01", "writer", "item"), 10, oldDay);
  for (const d of [path.join(base, "2026-10-01", "writer"), path.join(base, "2026-10-01")]) utimesSync(d, oldDay / 1000, oldDay / 1000);
  assert.deepEqual(names(), ["2026-10-01"], "untouched for the window: purged");
  // the writer dir inside the dated folder was written 2 h ago (an item moved in; the item itself kept its old age)
  utimesSync(path.join(base, "2026-10-01", "writer"), (now - 2 * 3600_000) / 1000, (now - 2 * 3600_000) / 1000);
  assert.deepEqual(names(), [], "a direct child written 2 h ago keeps the whole folder for 1 day");
  // the guard is `newest write <= now - days`: exactly a day old goes, a millisecond younger stays
  utimesSync(path.join(base, "2026-10-01", "writer"), (now - 86400_000) / 1000, (now - 86400_000) / 1000);
  assert.deepEqual(names(), ["2026-10-01"]);
  utimesSync(path.join(base, "2026-10-01", "writer"), (now - 86400_000 + 1000) / 1000, (now - 86400_000 + 1000) / 1000);
  assert.deepEqual(names(), []);
  // the folder's own mtime counts as well (an entry added or removed directly in it)
  utimesSync(path.join(base, "2026-10-01", "writer"), oldDay / 1000, oldDay / 1000);
  utimesSync(path.join(base, "2026-10-01"), (now - 3600_000) / 1000, (now - 3600_000) / 1000);
  assert.deepEqual(names(), []);
  // with 0 days nothing newer than `now` exists, so the emergency pass can empty a folder written an hour ago
  assert.deepEqual(names(0), ["2026-10-01"]);
});

test("purge: an item swept today is held a full window by the real moves, then goes (end to end through applySweep)", () => {
  const { root, policy } = fixture();
  policy.holdingDays = 1;
  file(path.join(root, "cache", "old", "f"), 10, OLD);
  const plan = planSweep({ policy, platform: "darwin", openPaths: [], size: () => 10 });
  const movedAt = new Date("2026-10-13T10:00:00Z"); // names the folder; the folder's mtime is the real clock
  assert.equal(applySweep({ plan, policy, platform: "darwin", date: movedAt }).results[0].ok, true);
  const base = path.join(root, "hold");
  const held = (now) => purgeHoldings({ policy, platform: "darwin", now, bases: [base], dryRun: true }).results.length;
  // the date alone would allow this (10-13 + 1 day), but the move just happened: held
  assert.equal(held(Date.now()), 0);
  assert.equal(held(Date.parse("2026-10-20T00:00:00Z")), 0, "a clock behind the move never purges what it cannot show to be a day old");
  assert.equal(held(Date.now() + 86400_000 + 1000), 1, "a day after the move it is purged");
});

test("committed policy: scratch idles one day, holding lasts one day, workspace-unknown keeps three", () => {
  const policy = readDiskPolicy();
  assert.deepEqual([policy.scratchStaleDays, policy.holdingDays, policy.unknownStaleDays], [1, 1, 3]);
});

// ---- .keep markers --------------------------------------------------------------------------------------------------------

test(".keep marker protects a stale idle unit for 14 days, then expires; a symlinked .keep protects nothing", () => {
  const { root, policy } = fixture();
  const scratch = path.join(root, "scratch");
  policy.scratchRoots = { mac: [scratch] };
  policy.scratchStaleDays = 1;
  const now = Date.now();
  const unit = (name, markerAgeDays) => {
    file(path.join(scratch, name, "data"), 10, OLD);
    if (markerAgeDays !== undefined) file(path.join(scratch, name, KEEP_MARKER), 0, now - markerAgeDays * DAY);
    utimesSync(path.join(scratch, name), OLD / 1000, OLD / 1000);
  };
  unit("kept", 13); unit("expired", 15); unit("plain");
  // a symlink named .keep, itself old, pointing at a fresh real file
  unit("linked");
  file(path.join(root, "elsewhere", "marker"), 0, now);
  symlinkSync(path.join(root, "elsewhere", "marker"), path.join(scratch, "linked", KEEP_MARKER));
  lutimesSync(path.join(scratch, "linked", KEEP_MARKER), OLD / 1000, OLD / 1000);
  utimesSync(path.join(scratch, "linked"), OLD / 1000, OLD / 1000);
  const plan = planSweep({ policy, platform: "darwin", now, openPaths: [], size: () => 10 });
  assert.deepEqual(plan.candidates.map((c) => path.basename(c.path)).sort(), ["expired", "linked", "plain"]);
  assert.equal(plan.skipped.find((s) => s.path.endsWith(`${path.sep}kept`)).reason, "kept");
  assert.match(renderSweep(plan), /skipped: 1 kept/);
  // the policy sets the marker lifetime
  policy.keepMarkerDays = 20;
  const longer = planSweep({ policy, platform: "darwin", now, openPaths: [], size: () => 10 });
  assert.deepEqual(longer.candidates.map((c) => path.basename(c.path)).sort(), ["linked", "plain"]);
  assert.equal(longer.skipped.find((s) => s.path.endsWith(`${path.sep}expired`)).reason, "kept");
});

test(".keep marker protects a sweep-writer unit and a workspace-unknown entry the same way", () => {
  const { ws, policy } = workspaceFixture();
  const now = Date.now();
  // kids: children of base/cache, idle past the 7-day default window
  const cache = policy.writers.find((w) => w.id === "kids").path;
  file(path.join(cache, "kept", "data"), 10, OLD); file(path.join(cache, "kept", KEEP_MARKER), 0, now - 13 * DAY); utimesSync(path.join(cache, "kept"), OLD / 1000, OLD / 1000);
  file(path.join(cache, "gone", "data"), 10, OLD);
  file(path.join(ws, "stale", KEEP_MARKER), 0, now - 1 * DAY); utimesSync(path.join(ws, "stale"), OLD / 1000, OLD / 1000);
  const plan = planSweep({ policy, platform: "darwin", now, openPaths: [], size: () => 10, workspaceSweep: true });
  assert.deepEqual(plan.candidates.map((c) => `${c.writer}:${path.basename(c.path)}`).sort(), ["kids:gone"]);
  assert.deepEqual(plan.skipped.filter((s) => s.reason === "kept").map((s) => path.basename(s.path)).sort(), ["kept", "stale"]);
});

test("keptByMarker: only a regular .keep file newer than the lifetime protects; a file unit and a missing marker do not", () => {
  const { root } = fixture();
  const now = Date.now();
  file(path.join(root, "u", KEEP_MARKER), 0, now - 3 * DAY);
  assert.equal(keptByMarker(path.join(root, "u"), now, 14), true);
  assert.equal(keptByMarker(path.join(root, "u"), now, 3), false, "exactly at the lifetime it has expired");
  assert.equal(keptByMarker(path.join(root, "u"), now, 2), false);
  assert.equal(keptByMarker(path.join(root, "nothing"), now, 14), false);
  file(path.join(root, "a-file"), 1, OLD);
  assert.equal(keptByMarker(path.join(root, "a-file"), now, 14), false);
  mkdirSync(path.join(root, "d", KEEP_MARKER), { recursive: true });
  assert.equal(keptByMarker(path.join(root, "d"), now, 14), false, "a directory named .keep is not a marker");
});

test("run logs how many units were kept by a marker", async () => {
  const { root, heavy, policy } = fixture();
  const pf = path.join(root, "policy.json");
  writeFileSync(pf, JSON.stringify(policy));
  file(path.join(root, "cache", "kept", "f"), 10, OLD);
  file(path.join(root, "cache", "kept", KEEP_MARKER), 0, Date.now() - DAY);
  utimesSync(path.join(root, "cache", "kept"), OLD / 1000, OLD / 1000);
  file(path.join(root, "cache", "old", "f"), 10, OLD);
  await runDiskCommand(["run", "--policy", pf], { platform: "darwin", stdout: () => {}, deps: { selfCheck: () => ({ ok: true, problems: [] }), openPaths: [], plan: { size: () => 10 }, check: { size: () => 0, space: () => ({ freeBytes: 5000, totalBytes: 10000 }) }, purge: { bases: [] } } });
  const events = readFileSync(path.join(heavy, "disk-hygiene.jsonl"), "utf8").trim().split("\n").map((l) => JSON.parse(l));
  const done = events.find((e) => e.event === "disk.run.ok");
  assert.equal(done.kept, 1);
  assert.equal(existsSync(path.join(root, "cache", "kept", "f")), true);
  assert.equal(existsSync(path.join(root, "cache", "old")), false);
});

// ---- stray cargo targets --------------------------------------------------------------------------------------------------

// A cargo target directory aged `mtimeMs`: debug/ output and, unless untagged, the CACHEDIR.TAG cargo writes at its root.
function cargoTarget(dir, mtimeMs, { tagged = true } = {}) {
  file(path.join(dir, "debug", "build.o"), 10, mtimeMs);
  if (tagged) {
    writeFileSync(path.join(dir, "CACHEDIR.TAG"), `${CACHEDIR_SIGNATURE}\n# This file is a cache directory tag created by cargo.\n`);
    utimesSync(path.join(dir, "CACHEDIR.TAG"), mtimeMs / 1000, mtimeMs / 1000);
  }
  utimesSync(dir, mtimeMs / 1000, mtimeMs / 1000);
}

function strayFixture() {
  const { root, policy } = fixture();
  const ws = path.join(root, "ws");
  const now = Date.now();
  cargoTarget(path.join(ws, "a", "target"), OLD);
  cargoTarget(path.join(ws, "b", "target"), OLD, { tagged: false });
  cargoTarget(path.join(ws, "c", "target"), now - 3600_000);
  cargoTarget(path.join(ws, "d", ".cache", "cargo-target"), OLD);
  policy.writers = [];
  policy.protectedSegments = ["Application Support", ".rightkit-managed"];
  policy.strayTargets = { mac: [{ path: ws, depth: 4 }] };
  policy.strayTargetDays = 1;
  return { root, ws, policy, now };
}

test("stray cargo targets: tagged, idle, untracked targets move; untagged ones, tracked ones, fresh ones and symlinks stay", () => {
  const { root, ws, policy, now } = strayFixture();
  // a repository whose target holds a tracked file, a symlinked target, and a tagged target below a protected name
  const repo = path.join(ws, "e");
  cargoTarget(path.join(repo, "target"), OLD);
  gitIn(repo, "init", "-q");
  gitIn(repo, "add", "-f", "target/CACHEDIR.TAG");
  gitIn(repo, "-c", "user.email=a@b", "-c", "user.name=n", "commit", "-qm", "track the tag");
  utimesSync(path.join(repo, "target"), OLD / 1000, OLD / 1000);
  cargoTarget(path.join(root, "elsewhere", "target"), OLD);
  mkdirSync(path.join(ws, "f"), { recursive: true });
  symlinkSync(path.join(root, "elsewhere", "target"), path.join(ws, "f", "target"));
  cargoTarget(path.join(ws, ".rightkit-managed", "x", "target"), OLD);
  const plan = planSweep({ policy, platform: "darwin", now, openPaths: [], size: () => 10, scan: () => ({ unknown: [], skipped: [] }) });
  assert.deepEqual(plan.candidates.map((c) => [c.writer, path.relative(ws, c.path)]), [["stray-cargo-target", path.join("a", "target")], ["stray-cargo-target", path.join("d", ".cache", "cargo-target")]]);
  const reason = (rel) => plan.skipped.find((s) => s.path === path.join(ws, rel))?.reason;
  assert.equal(reason(path.join("c", "target")), "fresh");
  assert.equal(reason(path.join("e", "target")), "tracked");
  for (const unseen of [path.join("b", "target"), path.join("f", "target"), path.join(".rightkit-managed", "x", "target")]) {
    assert.equal(plan.skipped.some((s) => s.path === path.join(ws, unseen)) || plan.candidates.some((c) => c.path === path.join(ws, unseen)), false, `${unseen} is not even considered`);
  }
  // `--days 0` takes the fresh one as well, but never the tracked one
  const all = planSweep({ policy, platform: "darwin", now, staleDays: 0, openPaths: [], size: () => 10, scan: () => ({ unknown: [], skipped: [] }) });
  assert.deepEqual(all.candidates.map((c) => path.relative(ws, c.path)), [path.join("a", "target"), path.join("c", "target"), path.join("d", ".cache", "cargo-target")]);
  // an open handle blocks it, and apply moves a stray into holding on its own volume
  const busy = planSweep({ policy, platform: "darwin", now, openPaths: [path.join(ws, "a", "target", "debug", "build.o")], size: () => 10, scan: () => ({ unknown: [], skipped: [] }) });
  assert.equal(busy.skipped.find((s) => s.path === path.join(ws, "a", "target")).reason, "in-use");
  policy.holding = { mac: [{ prefix: root, path: path.join(root, "hold") }] };
  const applied = applySweep({ plan, policy, platform: "darwin", date: new Date("2026-10-15T12:00:00Z") });
  assert.ok(applied.results.every((r) => r.ok));
  assert.equal(existsSync(path.join(ws, "a", "target")), false);
  assert.equal(existsSync(path.join(root, "hold", "2026-10-15", "stray-cargo-target", "target", "CACHEDIR.TAG")), true);
  assert.equal(existsSync(path.join(ws, "b", "target", "debug", "build.o")), true);
  assert.equal(existsSync(path.join(ws, "e", "target", "CACHEDIR.TAG")), true);
});

test("findStrayTargets: depth-bounded, real directories only, never inside a target, node_modules, .git or a protected name", () => {
  const { root } = fixture();
  const ws = path.join(root, "ws");
  const now = Date.now();
  cargoTarget(path.join(ws, "1", "target"), now);
  cargoTarget(path.join(ws, "1", "2", "3", "target"), now);          // depth 4: found
  cargoTarget(path.join(ws, "1", "2", "3", "4", "target"), now);     // depth 5: too deep
  cargoTarget(path.join(ws, "n", "node_modules", "pkg", "target"), now);
  cargoTarget(path.join(ws, "g", ".git", "x", "target"), now);
  cargoTarget(path.join(ws, "p", "Application Support", "target"), now);
  cargoTarget(path.join(ws, "1", "target", "nested", "target"), now); // inside a found target
  cargoTarget(path.join(ws, "u", "target"), now, { tagged: false });
  cargoTarget(path.join(ws, "u", "target", "inner", "target"), now);  // inside an untagged target
  cargoTarget(path.join(ws, "w", ".cache", "cargo-target"), now);
  cargoTarget(path.join(ws, "w", "cargo-target"), now);               // cargo-target counts only inside .cache
  // a tag with another signature, an empty tag and a symlinked tag are not cargo's
  cargoTarget(path.join(ws, "x", "target"), now, { tagged: false });
  writeFileSync(path.join(ws, "x", "target", "CACHEDIR.TAG"), "Signature: ffffffffffffffffffffffffffffffff\n");
  cargoTarget(path.join(ws, "y", "target"), now, { tagged: false });
  writeFileSync(path.join(ws, "y", "target", "CACHEDIR.TAG"), "");
  cargoTarget(path.join(ws, "z", "target"), now, { tagged: false });
  symlinkSync(path.join(ws, "1", "target", "CACHEDIR.TAG"), path.join(ws, "z", "target", "CACHEDIR.TAG"));
  symlinkSync(path.join(ws, "1"), path.join(ws, "linkdir"));         // a linked directory is not walked
  const rel = (list) => list.map((p) => path.relative(ws, p));
  assert.deepEqual(rel(findStrayTargets(ws, { depth: 4, protectedSegments: ["Application Support"] })), [path.join("1", "2", "3", "target"), path.join("1", "target"), path.join("w", ".cache", "cargo-target")]);
  assert.deepEqual(rel(findStrayTargets(ws, { depth: 2, protectedSegments: ["Application Support"] })), [path.join("1", "target")]);
  assert.deepEqual(findStrayTargets(path.join(ws, "1"), { depth: 4, protectedSegments: ["1"] }), [], "a root that is itself under a protected name yields nothing");
  assert.deepEqual(findStrayTargets(path.join(root, "absent")), []);
});

test("stray cargo targets: a unit inside a scratch run moves with the run, and a .keep on the run keeps its target", () => {
  const { root, policy, now } = strayFixture();
  const scratch = path.join(root, "scratch");
  policy.scratchRoots = { mac: [scratch] };
  policy.scratchStaleDays = 1;
  policy.strayTargets = { mac: [{ path: scratch, depth: 4 }] };
  for (const name of ["run-old", "run-kept"]) {
    cargoTarget(path.join(scratch, name, "repo", "target"), OLD);
    file(path.join(scratch, name, "repo", "src", "main.rs"), 5, OLD);
    utimesSync(path.join(scratch, name, "repo"), OLD / 1000, OLD / 1000);
    utimesSync(path.join(scratch, name), OLD / 1000, OLD / 1000);
  }
  file(path.join(scratch, "run-kept", KEEP_MARKER), 0, now - DAY);
  utimesSync(path.join(scratch, "run-kept"), OLD / 1000, OLD / 1000);
  const plan = planSweep({ policy, platform: "darwin", now, openPaths: [], size: () => 10, scan: () => ({ unknown: [], skipped: [] }) });
  assert.deepEqual(plan.candidates.map((c) => [c.writer, path.relative(scratch, c.path)]), [["scratch-root", "run-old"]], "the run moves whole; its target is not moved a second time");
  assert.equal(plan.skipped.find((s) => s.path.endsWith(path.join("run-old", "repo", "target"))).reason, "nested");
  assert.equal(plan.skipped.find((s) => s.path === path.join(scratch, "run-kept")).reason, "kept");
  assert.equal(plan.skipped.find((s) => s.path.endsWith(path.join("run-kept", "repo", "target"))).reason, "kept");
});

test("check lists stray cargo targets with size and age and warns for each", () => {
  const { ws, policy, now } = strayFixture();
  const r = checkDisk({ policy, platform: "darwin", now, size: () => 4096, space: () => ({ freeBytes: 5000, totalBytes: 10000 }), scan: () => ({ unknown: [], skipped: [] }) });
  assert.deepEqual(r.strays.map((s) => [path.relative(ws, s.path), s.bytes, s.ageDays >= 29 || s.ageDays === 0]), [[path.join("a", "target"), 4096, true], [path.join("c", "target"), 4096, true], [path.join("d", ".cache", "cargo-target"), 4096, true]]);
  assert.equal(r.warnings.filter((w) => w.startsWith("stray cargo target")).length, 3);
  assert.equal(r.warn, true);
  assert.match(renderCheck(r), /Stray cargo targets \(3\):\n +4 KB +\d+d +.*target/);
  const none = checkDisk({ policy: { ...policy, strayTargets: undefined }, platform: "darwin", now, size: () => 0, space: () => ({ freeBytes: 5000, totalBytes: 10000 }), scan: () => ({ unknown: [], skipped: [] }) });
  assert.deepEqual(none.strays, []);
});

test("committed policy: stray target roots on both hosts, one day idle; readDiskPolicy rejects a malformed entry", () => {
  const policy = readDiskPolicy();
  assert.equal(policy.strayTargetDays, 1);
  assert.deepEqual(policy.strayTargets.mac, [{ path: "/Volumes/D/claude", depth: 4 }, { path: "/Volumes/D/scratch", depth: 4 }]);
  assert.deepEqual(policy.strayTargets.win, [{ path: "D:\\claude", depth: 4 }, { path: "D:\\scratch", depth: 4 }]);
  const { root, policy: fixturePolicy } = fixture();
  const pf = path.join(root, "bad.json");
  for (const entry of [{ depth: 4 }, { path: "/x", depth: 0 }, { path: "/x", depth: 99 }]) {
    writeFileSync(pf, JSON.stringify({ ...fixturePolicy, strayTargets: { mac: [entry] } }));
    assert.throws(() => readDiskPolicy(pf), /strayTargets/);
  }
});

// ---- emergency purge -------------------------------------------------------------------------------------------------------

// A policy whose only floor is the agent volume (or, with agentVolume false, an ordinary volume) and a holding base with a
// dated folder for "today" holding bytes, so the run's purge decision is the only thing that can remove it.
function emergencyFixture({ agentVolume = true } = {}) {
  const { root, heavy, policy } = fixture();
  policy.floors = { mac: [{ id: "d", path: root, label: "fixture", warnFreeBytes: 1000, ...(agentVolume ? { agentVolume: true } : {}) }] };
  const pf = path.join(root, "policy.json");
  writeFileSync(pf, JSON.stringify(policy));
  const base = path.join(root, "hold");
  const now = Date.parse("2026-10-15T12:00:00Z");
  const today = path.join(base, "2026-10-15");
  file(path.join(today, "moved", "f"), 10, now - 3600_000);
  utimesSync(today, (now - 3600_000) / 1000, (now - 3600_000) / 1000); // the injected clock is the fixture's "now", not the real one
  writeFileSync(path.join(base, HOLDING_MARKER), "");
  const lines = [];
  const run = (freeBytes) => runDiskCommand(["run", "--policy", pf], { platform: "darwin", stdout: (l) => lines.push(l), deps: { selfCheck: () => ({ ok: true, problems: [] }), openPaths: [], plan: { size: () => 10 }, check: { size: () => 0, space: () => ({ freeBytes, totalBytes: 10000 }) }, purge: { bases: [base], now } } });
  const events = () => readFileSync(path.join(heavy, "disk-hygiene.jsonl"), "utf8").trim().split("\n").map((l) => JSON.parse(l));
  const calibrateRun = (freeBytes, args = []) => runDiskCommand(["check", "--calibrate", ...args, "--policy", pf], { platform: "darwin", stdout: (l) => lines.push(l), deps: { check: { space: () => ({ freeBytes, totalBytes: 10000 }) } } });
  return { root, heavy, pf, today, run, calibrateRun, events, lines };
}

// The line `disk check --calibrate` writes: every floor of the policy with the threshold it had then.
function calibrationLine(floors = [{ id: "d", label: "fixture", thresholdBytes: 1000, freeBytes: 5000, agentVolume: true }], ts = "2026-10-14T09:00:00.000Z") {
  return `${JSON.stringify({ ts, event: "disk.calibrated", floors })}\n`;
}
function calibrate(heavy, ...args) {
  mkdirSync(heavy, { recursive: true });
  appendFileSync(path.join(heavy, "disk-hygiene.jsonl"), calibrationLine(...args));
}

test("run: below the agent volume's soft floor the holding folders are purged the same day and the event says so", async () => {
  const { heavy, today, run, events } = emergencyFixture();
  calibrate(heavy);
  const r = await run(1);
  assert.equal(existsSync(today), false);
  assert.equal(r.purged.results.length, 1);
  const e = events().find((x) => x.event === "disk.run.emergency");
  assert.deepEqual([e.floor, e.freeBytes], ["d", 1]);
  assert.ok(e.purgedBytes > 0);
});

test("run: above the soft floor today's holding folder is kept and no emergency is logged", async () => {
  const { today, run, events } = emergencyFixture();
  await run(5000);
  assert.equal(existsSync(today), true);
  assert.equal(events().some((x) => x.event === "disk.run.emergency"), false);
});

test("run: a floor that is not the agent volume warning is not an emergency", async () => {
  const { today, run, events } = emergencyFixture({ agentVolume: false });
  const r = await run(1);
  assert.equal(r.exitCode, 3);
  assert.equal(existsSync(today), true);
  assert.equal(events().some((x) => x.event === "disk.run.emergency"), false);
});

// ---- emergency purge needs a recorded calibration (`disk check --calibrate`) ----------------------------------------------------

const SKIP_TEXT = /EMERGENCY SKIPPED \(([a-z-]+)\): fixture is below its floor but no calibration is recorded; run `right-release disk check --json`, compare freeBytes with the floor, then `right-release disk check --calibrate`/;

test("run: an uncalibrated emergency purges nothing and logs the skip, in the full pass", async () => {
  const { today, run, events, lines } = emergencyFixture();
  const r = await run(1);
  assert.equal(r.exitCode, 3);
  assert.equal(existsSync(today), true); // today's folder is inside its normal holding window
  const skipped = events().filter((e) => e.event === "disk.emergency.skipped");
  assert.equal(skipped.length, 1);
  assert.deepEqual([skipped[0].reason, skipped[0].floorId, skipped[0].freeBytes, skipped[0].thresholdBytes], ["uncalibrated", "d", 1, 1000]);
  assert.equal(events().some((e) => e.event === "disk.run.emergency"), false);
  assert.equal(SKIP_TEXT.exec(lines.join("\n"))?.[1], "uncalibrated");
  assert.doesNotMatch(lines.join("\n"), /EMERGENCY: the agent volume/);
});

test("run: an uncalibrated emergency otherwise runs as an ordinary pass: the normal holding window purges what has aged out", async () => {
  const { base, run, events } = lightFixture({ freeBytes: 1 });
  const today = path.join(base, "2026-10-15");
  file(path.join(today, "moved", "f"), 10, Date.now());
  const r = await run([], { plan: { size: () => 10 }, check: { size: () => 0, space: () => ({ freeBytes: 1, totalBytes: 10000 }) } });
  assert.equal(r.pass, "full");
  assert.equal(existsSync(path.join(base, "2020-01-01")), false); // past its window: the ordinary purge, not the emergency one
  assert.equal(existsSync(today), true); // inside the window: an emergency would have taken it
  assert.equal(events().find((e) => e.event === "disk.emergency.skipped").reason, "uncalibrated");
});

test("run --light: an uncalibrated emergency purges nothing, logs the skip and still sweeps normally", async () => {
  const { root, base, run, events, lines } = lightFixture({ freeBytes: 1 });
  const today = path.join(base, "2026-10-15");
  file(path.join(today, "moved", "f"), 10, Date.parse("2026-10-15T11:00:00Z"));
  const r = await run(["--light"], { purge: { bases: [base], now: Date.parse("2026-10-15T12:00:00Z") } });
  assert.equal(r.exitCode, 0);
  assert.equal(r.purged, null);
  assert.equal(existsSync(today), true);
  assert.equal(existsSync(path.join(base, "2020-01-01")), true);
  assert.equal(existsSync(path.join(root, "ltmp", "old")), false); // the light sweep itself ran as always
  const e = events();
  assert.equal(e.some((x) => x.event === "disk.run.emergency"), false);
  assert.deepEqual([e.find((x) => x.event === "disk.emergency.skipped").reason, e.find((x) => x.event === "disk.emergency.skipped").floorId], ["uncalibrated", "d"]);
  assert.equal(e.at(-1).event, "disk.run.light");
  assert.equal(e.at(-1).emergency, false);
  assert.match(lines.join("\n"), SKIP_TEXT);
});

test("check --calibrate while the agent volume is below its floor records nothing, names the floor and exits 1", async () => {
  const { heavy, calibrateRun, lines } = emergencyFixture();
  const r = await calibrateRun(1);
  assert.equal(r.exitCode, 1);
  assert.equal(existsSync(path.join(heavy, "disk-hygiene.jsonl")), false);
  const text = lines.join("\n");
  assert.match(text, /fixture: .* free, .* below its floor of /);
  assert.match(text, /lower the floor in disk-policy\.json or free space first/);
  assert.doesNotMatch(text, /Calibration recorded/);
});

test("check --calibrate with the agent volume above its floor records every floor and exits 0", async () => {
  const { root, heavy, calibrateRun, lines, run, today, events } = emergencyFixture();
  const r = await calibrateRun(5000);
  assert.equal(r.exitCode, 0);
  const line = events().find((e) => e.event === "disk.calibrated");
  assert.deepEqual(line.floors, [{ id: "d", label: "fixture", path: root, thresholdBytes: 1000, freeBytes: 5000, agentVolume: true }]);
  assert.match(lines.join("\n"), /Calibration recorded in .*disk-hygiene\.jsonl:\n {2}fixture: .* free, floor .* \(agent volume\)/);
  // the line it wrote is the one the emergency looks for
  await run(1);
  assert.equal(existsSync(today), false);
  assert.equal(events().some((e) => e.event === "disk.run.emergency"), true);
  assert.equal(events().some((e) => e.event === "disk.emergency.skipped"), false);
  assert.ok(existsSync(heavy));
});

test("check --calibrate: only an agent volume below its floor blocks it, and an unwritable log is not reported as recorded", async () => {
  const { root, heavy, policy } = fixture();
  const internal = path.join(root, "internal");
  mkdirSync(internal);
  policy.floors = { mac: [{ id: "internal", path: internal, label: "internal", warnFreeBytes: 1000, holdingSource: true }, { id: "d", path: root, label: "D", warnFreeBytes: 1000, agentVolume: true }] };
  const pf = path.join(root, "policy.json");
  writeFileSync(pf, JSON.stringify(policy));
  const space = (p) => (p === internal ? { freeBytes: 1, totalBytes: 10000 } : { freeBytes: 5000, totalBytes: 10000 });
  const lines = [];
  const calibrateRun = () => runDiskCommand(["check", "--calibrate", "--json", "--policy", pf], { platform: "darwin", stdout: (l) => lines.push(l), deps: { check: { space } } });
  const r = await calibrateRun();
  assert.equal(r.exitCode, 0); // the internal drive warning is not the agent volume's
  const out = JSON.parse(lines[0]);
  assert.equal(out.calibrated, true);
  assert.deepEqual(out.floors.map((f) => [f.id, f.thresholdBytes, f.freeBytes, f.agentVolume]), [["internal", 1000, 1, false], ["d", 1000, 5000, true]]);
  rmSync(heavy, { recursive: true });
  writeFileSync(heavy, "a file where the log directory should be");
  lines.length = 0;
  const failed = await calibrateRun();
  assert.equal(failed.exitCode, 1);
  assert.equal(JSON.parse(lines[0]).calibrated, false);
});

test("--calibrate is only valid with check", async () => {
  const { pf } = emergencyFixture();
  for (const command of ["sweep", "purge", "run"]) {
    await assert.rejects(runDiskCommand([command, "--calibrate", "--policy", pf], { platform: "darwin", stdout: () => {} }), /--calibrate is only valid with check/);
  }
});

test("run: changing a floor's threshold, or adding an agent volume floor, makes the calibration stale and the emergency is skipped", async () => {
  const lowered = emergencyFixture();
  calibrate(lowered.heavy, [{ id: "d", label: "fixture", thresholdBytes: 500, freeBytes: 5000, agentVolume: true }]); // calibrated at 500, the policy now says 1000
  await lowered.run(1);
  assert.equal(existsSync(lowered.today), true);
  assert.equal(lowered.events().find((e) => e.event === "disk.emergency.skipped").reason, "calibration-stale");
  assert.equal(lowered.events().some((e) => e.event === "disk.run.emergency"), false);
  // a calibration that does not list the floor at all is stale as well
  const other = emergencyFixture();
  calibrate(other.heavy, [{ id: "elsewhere", label: "x", thresholdBytes: 1000, freeBytes: 5000, agentVolume: true }]);
  await other.run(1);
  assert.equal(existsSync(other.today), true);
  assert.equal(other.events().find((e) => e.event === "disk.emergency.skipped").reason, "calibration-stale");
  // a calibration that matches (an older line included) is honoured
  const ok = emergencyFixture();
  calibrate(ok.heavy);
  calibrate(ok.heavy, [{ id: "d", label: "fixture", thresholdBytes: 500, freeBytes: 5000, agentVolume: true }], "2026-10-15T09:00:00.000Z");
  await ok.run(1);
  assert.equal(existsSync(ok.today), false);
});

test("run: a run log that cannot be read is uncalibrated, never calibrated", async () => {
  const { heavy, today, run, lines } = emergencyFixture();
  mkdirSync(path.join(heavy, "disk-hygiene.jsonl"), { recursive: true }); // EISDIR on read
  const r = await run(1);
  assert.equal(r.exitCode, 3);
  assert.equal(existsSync(today), true);
  assert.equal(SKIP_TEXT.exec(lines.join("\n"))?.[1], "log-unreadable");
  // ... while a log that does not exist yet is simply uncalibrated
  const fresh = emergencyFixture();
  await fresh.run(1);
  assert.equal(fresh.events().find((e) => e.event === "disk.emergency.skipped").reason, "uncalibrated");
});

test("run: a calibration under more than 4 MiB of later sweep lines is still found", async () => {
  const { heavy, today, run, events } = emergencyFixture();
  calibrate(heavy);
  const log = path.join(heavy, "disk-hygiene.jsonl");
  appendFileSync(log, fillerLine("2026-10-15T10:00:00.000Z").repeat(12000));
  assert.ok(statSync(log).size > 4 * 1024 * 1024, "the filler pushes the calibration out of the last 4 MiB");
  await run(1);
  assert.equal(existsSync(today), false);
  assert.equal(events().some((e) => e.event === "disk.emergency.skipped"), false);
});

test("schedule status says whether a calibration is recorded and how old it is", async () => {
  const none = statusFixture([{ event: "disk.run.ok", pass: "full", ago: 3 }]);
  assert.match((await none.status()).text, /calibration: NOT RECORDED \(emergency purge disabled\)/);
  const done = statusFixture([{ event: "disk.run.ok", pass: "full", ago: 3 }]);
  appendFileSync(done.logFile, calibrationLine(undefined, new Date(Date.parse("2026-10-24T12:00:00Z") - 2 * 3600_000).toISOString()));
  const r = await done.status();
  assert.match(r.text, /calibration: recorded 2\.0 h ago/);
  assert.doesNotMatch(r.text, /NOT RECORDED/);
  assert.equal(r.exitCode, 0); // an uncalibrated job is a notice, not a failed job
  const unreadable = statusFixture(null);
  mkdirSync(unreadable.logFile); // EISDIR
  assert.match((await unreadable.status()).text, /PROBLEM: the run log/);
});

// ---- hourly job: fullPassDue, `run --auto|--light` -------------------------------------------------------------------------

// Local-clock times: the full pass hour is the host's local 03:30.
const local = (day, hour, minute, second = 0) => new Date(2026, 9, day, hour, minute, second).getTime();

test("fullPassDue: one full pass a day after 03:30, a missed fire runs full at the next trigger, no pass on record is due", () => {
  assert.equal(fullPassDue({ now: local(24, 3, 29), lastFullMs: local(23, 3, 31) }), false); // yesterday's pass is still the latest slot
  assert.equal(fullPassDue({ now: local(24, 3, 31), lastFullMs: local(23, 3, 31) }), true);
  assert.equal(fullPassDue({ now: local(24, 3, 30), lastFullMs: local(23, 3, 31) }), true); // the slot itself counts
  assert.equal(fullPassDue({ now: local(24, 9, 0), lastFullMs: local(24, 3, 32) }), false); // after a pass the rest of the day is light
  assert.equal(fullPassDue({ now: local(24, 23, 59), lastFullMs: local(24, 3, 32) }), false);
  assert.equal(fullPassDue({ now: local(25, 9, 0), lastFullMs: local(23, 3, 31) }), true); // a whole day missed
  assert.equal(fullPassDue({ now: local(24, 9, 0), lastFullMs: local(24, 3, 29, 59) }), true); // a pass just before the slot does not count
  assert.equal(fullPassDue({ now: local(24, 9, 0), lastFullMs: null }), true);
  assert.equal(fullPassDue({ now: local(24, 9, 0), lastFullMs: undefined }), true);
  assert.equal(fullPassDue({ now: local(24, 9, 0), lastFullMs: local(24, 4, 10), hour: 5, minute: 0 }), true); // another slot time is honoured
});

// A light writer, a heavy one, a scratch root, a stray-target root and a workspace root (the last two only prove they are not read),
// and a purgeable holding folder: whatever the pass does to them shows in the tree.
function lightFixture({ freeBytes = 5000 } = {}) {
  const { root, heavy, policy } = fixture();
  policy.writers = [
    { id: "ltmp", platform: "mac", path: path.join(root, "ltmp"), capBytes: 100, action: "sweep", unit: "children", light: true },
    { id: "kids", platform: "mac", path: path.join(root, "cache"), capBytes: 1000, action: "sweep", unit: "children" },
    { id: "esc", platform: "mac", path: path.join(root, "esc"), capBytes: 1, action: "report", unit: "children", escalate: { factor: 1, staleDays: 0 } },
  ];
  policy.scratchRoots = { mac: [path.join(root, "scratch")] };
  policy.scratchStaleDays = 1;
  policy.strayTargets = { mac: [{ path: path.join(root, "ws"), depth: 4 }] };
  policy.workspaceRoots = { mac: [{ path: path.join(root, "ws") }] };
  policy.floors = { mac: [{ id: "d", path: root, label: "fixture", warnFreeBytes: 1000, agentVolume: true }] };
  const pf = path.join(root, "policy.json");
  writeFileSync(pf, JSON.stringify(policy));
  file(path.join(root, "ltmp", "old", "f"), 10, OLD);
  file(path.join(root, "ltmp", "new", "f"), 10);
  file(path.join(root, "cache", "old", "f"), 10, OLD);
  file(path.join(root, "esc", "old", "f"), 10, OLD);
  file(path.join(root, "scratch", "idle", "f"), 10, AGE(2));
  file(path.join(root, "scratch", "fresh", "f"), 10);
  const base = path.join(root, "hold");
  file(path.join(base, "2020-01-01", "x"), 10, OLD);
  writeFileSync(path.join(base, HOLDING_MARKER), "");
  const forbidden = () => { throw new Error("the light pass must not do this"); };
  const lines = [];
  const run = (args, extra = {}) => runDiskCommand(["run", ...args, "--policy", pf], { platform: "darwin", stdout: (l) => lines.push(l), deps: { selfCheck: () => ({ ok: true, problems: [] }), openPaths: [], plan: { size: () => 10, scan: forbidden, strays: forbidden }, check: { size: forbidden, space: () => ({ freeBytes, totalBytes: 10000 }) }, purge: { bases: [base] }, ...extra } });
  const events = () => readFileSync(path.join(heavy, "disk-hygiene.jsonl"), "utf8").trim().split("\n").map((l) => JSON.parse(l));
  return { root, heavy, base, run, events, lines };
}

test("run --light: no du, no workspace scan, no stray walk and no purge; only the light writers and the scratch roots are swept; it logs disk.run.light", async () => {
  const { root, base, run, events } = lightFixture();
  const r = await run(["--light"]);
  assert.equal(r.exitCode, 0);
  assert.equal(r.pass, "light");
  assert.equal(existsSync(path.join(root, "ltmp", "old")), false); // a light writer, idle past its window
  assert.equal(existsSync(path.join(root, "ltmp", "new")), true);
  assert.equal(existsSync(path.join(root, "scratch", "idle")), false); // scratch idle two days
  assert.equal(existsSync(path.join(root, "scratch", "fresh")), true);
  assert.equal(existsSync(path.join(root, "cache", "old")), true); // not a light writer
  assert.equal(existsSync(path.join(root, "esc", "old")), true); // escalation would size the whole root
  assert.equal(existsSync(path.join(base, "2020-01-01")), true); // purging belongs to the full pass
  const last = events().at(-1);
  assert.equal(last.event, "disk.run.light");
  assert.equal(last.pass, "light");
  assert.equal(last.emergency, false);
  assert.equal(last.movedBytes, 20);
  assert.deepEqual(last.floors, [{ id: "d", freeBytes: 5000, warn: false }]);
  assert.equal(last.freeBytes, 5000);
  assert.equal(events().some((e) => e.event === "disk.run.ok" || e.event === "disk.floor.warn" || e.event === "disk.report"), false);
});

test("run --light: exit 0 below a floor too (only a failed self-check fails it), and below the agent volume's floor it purges, as the full pass does", async () => {
  const { root, heavy, base, run, events } = lightFixture({ freeBytes: 1 });
  calibrate(heavy);
  const today = path.join(base, "2026-10-15");
  file(path.join(today, "moved", "f"), 10, Date.parse("2026-10-15T11:00:00Z"));
  utimesSync(today, Date.parse("2026-10-15T11:00:00Z") / 1000, Date.parse("2026-10-15T11:00:00Z") / 1000);
  const r = await run(["--light"], { purge: { bases: [base], now: Date.parse("2026-10-15T12:00:00Z") } });
  assert.equal(r.exitCode, 0);
  assert.equal(existsSync(today), false);
  assert.equal(existsSync(path.join(base, "2020-01-01")), false);
  assert.equal(existsSync(path.join(root, "cache", "old")), true); // the emergency purges holding folders, it does not widen the sweep
  const e = events();
  assert.equal(e.find((x) => x.event === "disk.run.emergency").floor, "d");
  const light = e.at(-1);
  assert.equal(light.event, "disk.run.light");
  assert.equal(light.emergency, true);
  assert.equal(light.floors[0].warn, true);
  const failed = await runDiskCommand(["run", "--light", "--policy", path.join(root, "policy.json")], { platform: "darwin", stdout: () => {}, deps: { selfCheck: () => ({ ok: false, problems: ["x"] }) } });
  assert.equal(failed.exitCode, 2);
});

test("run --light: lsof is read once and only when a candidate exists", async () => {
  const busy = lightFixture();
  let reads = 0;
  await busy.run(["--light"], { openPaths: undefined, listOpenPaths: () => { reads += 1; return []; } });
  assert.equal(reads, 1); // two candidates (ltmp/old, scratch/idle), one listing
  const idle = lightFixture();
  rmSync(path.join(idle.root, "ltmp"), { recursive: true });
  rmSync(path.join(idle.root, "scratch"), { recursive: true });
  let idleReads = 0;
  await idle.run(["--light"], { openPaths: undefined, listOpenPaths: () => { idleReads += 1; return []; } });
  assert.equal(idleReads, 0);
});

test("run --auto: light once a full pass finished after the last 03:30, full when none did (a missed fire, a fresh log)", async () => {
  const lastSlot = (day) => new Date(2026, 9, day, 3, 35).toISOString();
  const now = local(24, 9, 0);
  const seeded = async (ts) => {
    const { heavy, run, events } = lightFixture();
    mkdirSync(heavy, { recursive: true });
    if (ts) writeFileSync(path.join(heavy, "disk-hygiene.jsonl"), `${JSON.stringify({ ts, event: "disk.run.ok", pass: "full" })}\n`);
    const r = await run(["--auto"], { now, plan: { size: () => 10 }, check: { size: () => 0, space: () => ({ freeBytes: 5000, totalBytes: 10000 }) } });
    return { r, last: events().filter((e) => e.event === "disk.run.light" || e.event === "disk.run.ok" || e.event === "disk.floor.warn").at(-1) };
  };
  const light = await seeded(lastSlot(24));
  assert.equal(light.last.event, "disk.run.light");
  assert.equal(light.r.pass, "light");
  const missed = await seeded(lastSlot(23));
  assert.equal(missed.last.pass, "full"); // the fixture's uncovered top-level entries make it a warning line, still a full pass
  const fresh = await seeded(null);
  assert.equal(fresh.r.pass, "full");
  // a legacy line without `pass` is a full pass; a light line never satisfies the requirement
  const { heavy, run } = lightFixture();
  mkdirSync(heavy, { recursive: true });
  writeFileSync(path.join(heavy, "disk-hygiene.jsonl"), `${JSON.stringify({ ts: lastSlot(24), event: "disk.run.ok" })}\n${JSON.stringify({ ts: new Date(local(24, 8, 0)).toISOString(), event: "disk.run.light", pass: "light" })}\n`);
  assert.equal((await run(["--auto"], { now, plan: { size: () => 10 }, check: { size: () => 0, space: () => ({ freeBytes: 5000, totalBytes: 10000 }) } })).pass, "light");
  const lightOnly = lightFixture();
  mkdirSync(lightOnly.heavy, { recursive: true });
  writeFileSync(path.join(lightOnly.heavy, "disk-hygiene.jsonl"), `${JSON.stringify({ ts: new Date(local(24, 8, 0)).toISOString(), event: "disk.run.light", pass: "light" })}\n`);
  assert.equal((await lightOnly.run(["--auto"], { now, plan: { size: () => 10 }, check: { size: () => 0, space: () => ({ freeBytes: 5000, totalBytes: 10000 }) } })).pass, "full");
});

test("a bare `disk run` is the full pass: it logs a full-pass line and purges; --auto and --light exclude each other", async () => {
  const { base, run, events } = lightFixture();
  const r = await run([], { plan: { size: () => 10 }, check: { size: () => 0, space: () => ({ freeBytes: 5000, totalBytes: 10000 }) } });
  assert.equal(r.pass, "full");
  assert.equal(existsSync(path.join(base, "2020-01-01")), false);
  const ok = events().find((e) => e.event === "disk.run.ok" || e.event === "disk.floor.warn");
  assert.equal(ok.pass, "full");
  await assert.rejects(() => run(["--auto", "--light"]), /either --auto or --light/);
});

test("committed policy: the cheap temp-like writers are the light ones, and every light writer sweeps", () => {
  const { writers } = readDiskPolicy();
  const light = writers.filter((w) => w.light).map((w) => `${w.platform}:${w.id}`).sort();
  assert.deepEqual(light, ["mac:agent-tmp", "mac:build-home-tmp", "mac:claude-session-dirs", "mac:rightkit-qa-tmp", "win:agent-tmp", "win:build-home-tmp", "win:win-temp"]);
  for (const w of writers.filter((x) => x.light)) assert.equal(w.action, "sweep", w.id);
});

// ---- schedule status: last-run ages, a problem past 26 h -------------------------------------------------------------------

// A schedule bound to a copy under a stable root, a log with the given events (`ago` hours before `now`), nothing read from the host.
function statusFixture(events) {
  const base = mkdtempSync(path.join(os.tmpdir(), "rk-status-age-"));
  const home = path.join(base, "home");
  const installed = installStableRoot({ cli: fakeInstalled(base, "1.0.0"), platform: "darwin", home });
  const now = Date.parse("2026-10-24T12:00:00Z");
  const logFile = path.join(base, "disk-hygiene.jsonl");
  if (events) writeFileSync(logFile, events.map((e) => `${JSON.stringify({ ts: new Date(now - e.ago * 3600_000).toISOString(), event: e.event, ...(e.pass ? { pass: e.pass } : {}) })}\n`).join(""));
  const status = async ({ binding = { node: process.execPath, cli: installed.cli, source: "plist" }, loaded = true } = {}) => {
    const lines = [];
    const r = await runDiskCommand(["schedule", "status"], { platform: "darwin", stdout: (l) => lines.push(l), deps: { schedule: { binding, loaded, now, logFile, home } } });
    return { exitCode: r.exitCode, text: lines.join("\n") };
  };
  return { status, logFile };
}

test("schedule status prints the age of the last full and light run and passes within 26 h", async () => {
  const { status } = statusFixture([{ event: "disk.run.ok", pass: "full", ago: 3 }, { event: "disk.run.light", pass: "light", ago: 0.5 }]);
  const r = await status();
  assert.equal(r.exitCode, 0);
  assert.match(r.text, /last full run: 3\.0 h ago \(2026-10-24T09:00:00\.000Z, disk\.run\.ok\)/);
  assert.match(r.text, /last light run: 30 min ago/);
  assert.doesNotMatch(r.text, /PROBLEM/);
});

test("schedule status fails when the newest full run is over 26 h old, when there is none, or when only light runs exist", async () => {
  const old = await statusFixture([{ event: "disk.run.ok", ago: 27 }, { event: "disk.run.light", pass: "light", ago: 0.2 }]).status();
  assert.equal(old.exitCode, 1);
  assert.match(old.text, /PROBLEM: last completed full run was 27\.0 h ago \(limit 26 h\)/);
  const empty = await statusFixture([]).status();
  assert.equal(empty.exitCode, 1);
  assert.match(empty.text, /PROBLEM: no completed full run recorded/);
  const unreadable = await statusFixture(null).status();
  assert.equal(unreadable.exitCode, 1);
  const lightOnly = await statusFixture([{ event: "disk.run.light", pass: "light", ago: 0.2 }]).status();
  assert.equal(lightOnly.exitCode, 1);
  assert.match(lightOnly.text, /no completed full run recorded/);
  const warned = await statusFixture([{ event: "disk.floor.warn", pass: "full", ago: 25 }]).status();
  assert.equal(warned.exitCode, 0); // a full pass that ended in a warning still ran
});

test("schedule status: not installed is fine and reads no log; an unloaded job fails even with a fresh run", async () => {
  const { status } = statusFixture([{ event: "disk.run.ok", pass: "full", ago: 1 }]);
  const none = await status({ binding: null });
  assert.equal(none.exitCode, 0);
  assert.equal(none.text, "not installed");
  const unloaded = await status({ loaded: false });
  assert.equal(unloaded.exitCode, 1);
  assert.match(unloaded.text, /plist present but not loaded/);
});

test("evaluateRunAge: newest full and light events, the 26 h limit is inclusive of exactly 26 h, damaged timestamps are ignored", () => {
  const now = Date.parse("2026-10-24T12:00:00Z");
  const at = (hours, event, extra = {}) => ({ ts: new Date(now - hours * 3600_000).toISOString(), event, ...extra });
  const r = evaluateRunAge({ events: [at(30, "disk.run.ok"), at(5, "disk.floor.warn", { pass: "full" }), at(2, "disk.run.ok"), at(1, "disk.run.light", { pass: "light" }), at(0.5, "disk.sweep.move"), { event: "disk.run.ok", ts: "not a date" }], now });
  assert.equal(r.ok, true);
  assert.equal(r.lastFull.ts, at(2, "disk.run.ok").ts);
  assert.equal(r.lastLight.ts, at(1, "disk.run.light").ts);
  assert.equal(evaluateRunAge({ events: [at(26, "disk.run.ok")], now }).ok, true);
  assert.equal(evaluateRunAge({ events: [at(26.1, "disk.run.ok")], now }).ok, false);
  assert.deepEqual(evaluateRunAge({ events: [], now }).problems, ["no completed full run recorded"]);
});

// ---- the daily report line and RustRight's budget ---------------------------------------------------------------------------

test("run appends exactly one disk.report per UTC day on the full pass, none on the light pass; the next day's report carries a delta", async () => {
  const { run, events } = lightFixture();
  const full = (nowIso, used = 5000) => run([], { now: Date.parse(nowIso), plan: { size: () => 10 }, check: { size: () => 100, space: () => ({ freeBytes: 10000 - used, totalBytes: 10000 }) } });
  const reports = () => events().filter((e) => e.event === "disk.report");
  await full("2026-10-15T03:40:00Z");
  assert.equal(reports().length, 1);
  assert.equal(reports()[0].ts, "2026-10-15T03:40:00.000Z");
  assert.equal(reports()[0].delta, null);
  assert.equal(reports()[0].usedBytes, 5000);
  assert.equal(reports()[0].units["scratch/fresh"], 100); // one du per scratch child, through the same size seam as the check
  await full("2026-10-15T22:00:00Z"); // a second full pass the same UTC day
  assert.equal(reports().length, 1);
  await run(["--light"], { now: Date.parse("2026-10-16T04:00:00Z") });
  assert.equal(reports().length, 1);
  await full("2026-10-16T03:40:00Z", 5300);
  assert.equal(reports().length, 2);
  assert.equal(reports()[1].delta.usedBytes, 300);
  assert.equal(reports()[1].delta.hours, 24);
  assert.equal(Object.values(reports()[1].delta.categories).reduce((a, b) => a + b, 0), 300);
});

test("check: a writer with a budget takes the sum of its RustRight parts as its cap and says so; without RustRight the policy cap stays", () => {
  const { root, policy } = fixture();
  policy.writers = [
    { id: "bc-targets", platform: "mac", path: path.join(root, "bc"), capBytes: 100, action: "report", budget: ["total"] },
    { id: "bc-sccache", platform: "mac", path: path.join(root, "bc", "sccache"), capBytes: 50, action: "report", budget: ["sccache"] },
    { id: "bc-two", platform: "mac", path: path.join(root, "two"), capBytes: 1, action: "report", budget: ["targets", "trash"] },
    { id: "plain", platform: "mac", path: path.join(root, "plain"), capBytes: 5, action: "report" },
  ];
  const budget = { targets: 500, sccache: 200, cargoHome: 10, toolchains: 10, trash: 40, slack: 10, totalBytes: 900 };
  const size = () => 600;
  const space = () => ({ freeBytes: 5000, totalBytes: 10000 });
  for (const dir of ["bc", "two", "plain"]) mkdirSync(path.join(root, dir), { recursive: true });
  mkdirSync(path.join(root, "bc", "sccache"));
  let reads = 0;
  const r = checkDisk({ policy, platform: "darwin", size, space, scan: () => ({ unknown: [], skipped: [] }), readBudget: () => { reads += 1; return budget; } });
  assert.equal(reads, 1); // asked once for all rows
  const row = (id) => r.writers.find((w) => w.id === id);
  assert.deepEqual([row("bc-targets").capBytes, row("bc-targets").capSource, row("bc-targets").over], [900, "rustright", false]);
  assert.deepEqual([row("bc-sccache").capBytes, row("bc-sccache").capSource, row("bc-sccache").over], [200, "rustright", true]);
  assert.deepEqual([row("bc-two").capBytes, row("bc-two").capSource], [540, "rustright"]);
  assert.deepEqual([row("plain").capBytes, row("plain").capSource], [5, "policy"]);
  const without = checkDisk({ policy, platform: "darwin", size, space, scan: () => ({ unknown: [], skipped: [] }), readBudget: () => null });
  assert.deepEqual(without.writers.map((w) => [w.id, w.capBytes, w.capSource]), [["bc-targets", 100, "policy"], ["bc-sccache", 50, "policy"], ["bc-two", 1, "policy"], ["plain", 5, "policy"]]);
  assert.match(renderCheck(r), /bc-targets .*cap .* \(RustRight budget\)/);
  assert.doesNotMatch(renderCheck(without), /RustRight budget/);
});

test("check does not ask RustRight when no writer follows its budget", () => {
  const { policy } = fixture();
  let reads = 0;
  checkDisk({ policy, platform: "darwin", size: () => 0, space: () => ({ freeBytes: 5000, totalBytes: 10000 }), scan: () => ({ unknown: [], skipped: [] }), readBudget: () => { reads += 1; return null; } });
  assert.equal(reads, 0);
});

test("readRustrightBudget: `rustright doctor --json --no-scan` with a 20 s limit and no shell; any failure is null", () => {
  const budget = { targets: 160e9, sccache: 55e9, cargoHome: 10e9, toolchains: 10e9, trash: 5e9, slack: 10e9, totalBytes: 250e9, source: { targets: "default" } };
  const calls = [];
  const ok = readRustrightBudget({ run: (cmd, args, options) => { calls.push([cmd, args, options]); return { status: 0, stdout: JSON.stringify({ schema: 1, buildBudget: budget }) }; } });
  assert.deepEqual(ok, budget);
  assert.deepEqual([calls[0][0], calls[0][1]], ["rustright", ["doctor", "--json", "--no-scan"]]);
  assert.equal(calls[0][2].timeout, 20_000);
  assert.equal(calls[0][2].shell, false);
  const answer = (r) => readRustrightBudget({ run: () => r });
  assert.equal(answer({ status: 1, stdout: JSON.stringify({ buildBudget: budget }) }), null);
  assert.equal(answer({ status: null, error: Object.assign(new Error("spawnSync rustright ENOENT"), { code: "ENOENT" }), stdout: "" }), null);
  assert.equal(answer({ status: null, error: Object.assign(new Error("timed out"), { code: "ETIMEDOUT" }), stdout: "" }), null);
  assert.equal(answer({ status: 0, stdout: "not json" }), null);
  assert.equal(answer({ status: 0, stdout: JSON.stringify({ buildBudget: null }) }), null); // a config without a budget block
  assert.equal(answer({ status: 0, stdout: JSON.stringify({}) }), null);
  assert.equal(answer({ status: 0, stdout: JSON.stringify({ buildBudget: { ...budget, sccache: "55" } }) }), null);
  assert.equal(answer({ status: 0, stdout: JSON.stringify({ buildBudget: { ...budget, totalBytes: undefined } }) }), null);
  assert.equal(readRustrightBudget({ run: () => { throw new Error("boom"); } }), null);
});

test("readDiskPolicy rejects a budget that is not a non-empty list of RustRight parts", () => {
  const { root, policy } = fixture();
  const pf = path.join(root, "bad.json");
  for (const budget of [[], "total", ["nope"], [1], ["targets", "slackk"]]) {
    writeFileSync(pf, JSON.stringify({ ...policy, writers: [{ id: "x", platform: "mac", path: root, capBytes: 1, action: "report", budget }] }));
    assert.throws(() => readDiskPolicy(pf), /budget must list parts/);
  }
  writeFileSync(pf, JSON.stringify({ ...policy, writers: [{ id: "x", platform: "mac", path: root, capBytes: 1, action: "report", budget: ["targets", "trash", "total"] }] }));
  assert.equal(readDiskPolicy(pf).writers[0].budget.length, 3);
});

test("committed policy: the build-control rows follow RustRight's budget (total, sccache), the scratch total is the scratch category", () => {
  const { writers } = readDiskPolicy();
  for (const platform of ["mac", "win"]) {
    const row = (id) => writers.find((w) => w.id === id && w.platform === platform);
    assert.deepEqual(row("build-control-targets").budget, ["total"], platform);
    assert.equal(row("build-control-targets").capBytes, 250e9, platform); // the offline default when RustRight cannot be read
    assert.deepEqual((row("build-control-sccache") ?? row("win-sccache")).budget, ["sccache"], platform);
    assert.equal(row("scratch-total").category, "scratch", platform);
  }
});

// ---- run-root leases -------------------------------------------------------------------------------------------------------

// A scratch root with a total cap and per-owner caps; sizes come from a table keyed by the path relative to the root.
function leaseFixture({ sizes = {}, total = 1000, caps = { rk: 100, legion: 400 } } = {}) {
  const { root, policy } = fixture();
  const scratch = path.join(root, "scratch");
  mkdirSync(scratch);
  for (const name of Object.keys(sizes)) mkdirSync(path.join(scratch, name)); // the sizes table stands in for their contents
  policy.scratchRoots = { mac: [scratch] };
  policy.scratchStaleDays = 1;
  policy.scratchPrefixCaps = caps;
  policy.writers = [{ id: "scratch-total", platform: "mac", path: scratch, capBytes: total, action: "report" }];
  const pf = path.join(root, "policy.json");
  writeFileSync(pf, JSON.stringify(policy));
  const size = (p) => (p === scratch ? Object.values(sizes).reduce((a, b) => a + b, 0) : sizes[path.relative(scratch, p)] ?? 0);
  const now = Date.parse("2026-10-24T12:00:00Z");
  const cmd = async (args, extra = {}) => {
    const out = []; const err = [];
    const r = await runDiskCommand(["run-root", ...args, "--policy", pf], { platform: "darwin", stdout: (l) => out.push(l), deps: { runRoot: { size, now }, stderr: (l) => err.push(l), ...extra } });
    return { ...r, out: out.join("\n"), err: err.join("\n") };
  };
  return { root, scratch, policy, pf, size, now, cmd };
}

test("run-root writes the lease file the guard reads: schema 1, name, createdAt, leaseUntil, three days by default", async () => {
  const { scratch, now, cmd } = leaseFixture();
  const r = await cmd(["rk-build-1"]);
  assert.equal(r.exitCode, 0);
  const file = path.join(scratch, "rk-build-1", RUN_LEASE_FILE);
  assert.equal(RUN_LEASE_FILE, ".rightkit-run.json");
  const lease = JSON.parse(readFileSync(file, "utf8"));
  assert.deepEqual(lease, { schema: 1, name: "rk-build-1", createdAt: "2026-10-24T12:00:00.000Z", leaseUntil: "2026-10-27T12:00:00.000Z" });
  assert.deepEqual(Object.keys(lease), ["schema", "name", "createdAt", "leaseUntil"]);
  assert.equal(lstatSync(file).isFile(), true);
  assert.match(r.out, /leased run root .*rk-build-1 until 2026-10-27T12:00:00\.000Z/);
  const days = await cmd(["rk-long", "--days", "10"]);
  assert.equal(JSON.parse(readFileSync(path.join(scratch, "rk-long", RUN_LEASE_FILE), "utf8")).leaseUntil, "2026-11-03T12:00:00.000Z");
  assert.equal(days.exitCode, 0);
  const json = await cmd(["hr-j", "--json"]);
  assert.deepEqual(JSON.parse(json.out), { path: path.join(scratch, "hr-j"), schema: 1, name: "hr-j", createdAt: new Date(now).toISOString(), leaseUntil: "2026-10-27T12:00:00.000Z" });
});

test("run-root renews a lease and keeps its createdAt; --release removes the lease file and leaves the directory", async () => {
  const { scratch, now, cmd } = leaseFixture();
  await cmd(["rk-a"]);
  const later = await cmd(["rk-a", "--days", "1"], { runRoot: { size: () => 0, now: now + 86400_000 } });
  assert.match(later.out, /renewed run root/);
  assert.deepEqual(JSON.parse(readFileSync(path.join(scratch, "rk-a", RUN_LEASE_FILE), "utf8")), { schema: 1, name: "rk-a", createdAt: new Date(now).toISOString(), leaseUntil: "2026-10-26T12:00:00.000Z" });
  writeFileSync(path.join(scratch, "rk-a", "work.txt"), "x");
  const gone = await cmd(["--release", "rk-a"]);
  assert.equal(gone.exitCode, 0);
  assert.match(gone.out, /released/);
  assert.equal(existsSync(path.join(scratch, "rk-a", RUN_LEASE_FILE)), false);
  assert.equal(existsSync(path.join(scratch, "rk-a", "work.txt")), true);
  const again = await cmd(["rk-a", "--release"]);
  assert.equal(again.exitCode, 0);
  assert.match(again.out, /had no lease/);
  const missing = await cmd(["--release", "rk-nothing"]);
  assert.equal(missing.exitCode, 1);
  assert.match(missing.err, /no run root/);
});

test("run-root refuses a name that is not a single plain segment and writes nothing", async () => {
  const { scratch, cmd } = leaseFixture();
  for (const name of ["../escape", "a/b", "a\\b", ".hidden", "-x", "x y", "", "a".repeat(81), "dot."]) {
    const r = await cmd([name]);
    assert.equal(r.exitCode, 1, JSON.stringify(name));
    assert.match(r.err, /must be 1 to 80 letters/);
  }
  assert.deepEqual(readdirSync(scratch), []);
  await assert.rejects(() => cmd(["rk-x", "--days", "0"]).then((r) => { if (r.exitCode) throw new Error(r.err); }), /positive/);
  await assert.rejects(() => cmd(["rk-x", "--bogus"]), /unknown option/);
  await assert.rejects(() => cmd(["a", "b"]), /one run root name only/);
  await assert.rejects(() => cmd(["--release", "rk-x", "--days", "2"]), /takes no --days/);
});

test("run-root refuses when the scratch total is over its cap, or the name's owner prefix is over its cap; other owners are unaffected", async () => {
  const over = leaseFixture({ sizes: { "rk-1": 60, "rk-2": 60, "legion-1": 100 } });
  const prefix = await over.cmd(["rk-3"]);
  assert.equal(prefix.exitCode, 1);
  assert.match(prefix.err, /"rk" run roots hold .* over their .* cap; release or remove finished rk-\* run roots/);
  assert.equal(existsSync(path.join(over.scratch, "rk-3")), false);
  assert.equal((await over.cmd(["legion-9"])).exitCode, 0); // 100 of 400
  assert.equal((await over.cmd(["other-1"])).exitCode, 0); // no cap for this owner
  const full = leaseFixture({ sizes: { "legion-1": 900, "rk-1": 50, "x-1": 100 } });
  const total = await full.cmd(["legion-2"]);
  assert.equal(total.exitCode, 1);
  assert.match(total.err, /scratch holds .* over its .* total cap/);
  assert.equal(existsSync(path.join(full.scratch, "legion-2")), false);
  assert.equal((await full.cmd(["rk-2"])).exitCode, 1); // the total applies to every owner
  // exactly at the cap is not over it
  const edge = leaseFixture({ sizes: { "rk-1": 100 }, total: 100 });
  assert.equal((await edge.cmd(["rk-2"])).exitCode, 0);
});

test("run-root refuses a symlinked name and a lease path that is not a regular file", async () => {
  const { root, scratch, cmd } = leaseFixture();
  symlinkSync(root, path.join(scratch, "rk-link"));
  assert.equal((await cmd(["rk-link"])).exitCode, 1);
  mkdirSync(path.join(scratch, "rk-dirlease", RUN_LEASE_FILE), { recursive: true });
  const r = await cmd(["rk-dirlease"]);
  assert.equal(r.exitCode, 1);
  assert.match(r.err, /not a regular file/);
  assert.equal(existsSync(path.join(root, RUN_LEASE_FILE)), false);
});

test("createRunRoot and releaseRunRoot: no scratch root for the platform, or no scratch volume, is an error", () => {
  const { policy, root } = leaseFixture();
  assert.throws(() => createRunRoot({ policy: { ...policy, scratchRoots: {} }, platform: "darwin", name: "rk-a" }), /no scratch root/);
  assert.throws(() => createRunRoot({ policy: { ...policy, scratchRoots: { mac: [path.join(root, "no", "such", "scratch")] } }, platform: "darwin", name: "rk-a" }), /does not exist and cannot be created/);
  assert.throws(() => releaseRunRoot({ policy: { ...policy, scratchRoots: {} }, platform: "darwin", name: "rk-a" }), /no scratch root/);
});

// A scratch run with a lease file, its contents idle for `idleDays`.
function leasedRun(scratch, name, lease, idleDays = 5) {
  file(path.join(scratch, name, "f"), 10, Date.now() - idleDays * 86400_000);
  if (lease !== null) {
    writeFileSync(path.join(scratch, name, RUN_LEASE_FILE), typeof lease === "string" ? lease : JSON.stringify(lease));
    utimesSync(path.join(scratch, name, RUN_LEASE_FILE), (Date.now() - idleDays * 86400_000) / 1000, (Date.now() - idleDays * 86400_000) / 1000);
  }
  utimesSync(path.join(scratch, name), (Date.now() - idleDays * 86400_000) / 1000, (Date.now() - idleDays * 86400_000) / 1000);
}

const leaseOf = (name, untilMs) => ({ schema: 1, name, createdAt: new Date(untilMs - 3 * DAY).toISOString(), leaseUntil: new Date(untilMs).toISOString() });

test("sweep: a scratch run with a valid lease is skipped as leased however idle it is; an expired or unreadable lease protects nothing", () => {
  const { scratch, policy } = leaseFixture();
  const now = Date.now();
  leasedRun(scratch, "rk-live", leaseOf("rk-live", now + DAY));
  leasedRun(scratch, "rk-expired", leaseOf("rk-expired", now - 3600_000));
  leasedRun(scratch, "rk-plain", null);
  leasedRun(scratch, "rk-garbage", "{not json");
  leasedRun(scratch, "rk-schema2", { ...leaseOf("rk-schema2", now + DAY), schema: 2 });
  leasedRun(scratch, "rk-nodate", { schema: 1, name: "rk-nodate", createdAt: "x", leaseUntil: "y" });
  leasedRun(scratch, "rk-huge", JSON.stringify({ ...leaseOf("rk-huge", now + DAY), pad: "x".repeat(5000) }));
  // a symlinked lease file proves nothing, even when the target is a valid lease
  leasedRun(scratch, "rk-symlink", null);
  writeFileSync(path.join(scratch, "elsewhere.json"), JSON.stringify(leaseOf("rk-symlink", now + DAY)));
  symlinkSync(path.join(scratch, "elsewhere.json"), path.join(scratch, "rk-symlink", RUN_LEASE_FILE));
  lutimesSync(path.join(scratch, "rk-symlink", RUN_LEASE_FILE), (now - 5 * DAY) / 1000, (now - 5 * DAY) / 1000);
  utimesSync(path.join(scratch, "rk-symlink"), (now - 5 * DAY) / 1000, (now - 5 * DAY) / 1000);
  utimesSync(path.join(scratch, "elsewhere.json"), (now - 5 * DAY) / 1000, (now - 5 * DAY) / 1000);
  const plan = planSweep({ policy, platform: "darwin", now, openPaths: [], size: () => 10, scan: () => ({ unknown: [], skipped: [] }) });
  const moved = plan.candidates.filter((c) => c.writer === "scratch-root").map((c) => path.basename(c.path)).sort();
  assert.deepEqual(moved, ["elsewhere.json", "rk-expired", "rk-garbage", "rk-huge", "rk-nodate", "rk-plain", "rk-schema2", "rk-symlink"]);
  assert.deepEqual(plan.skipped.filter((s) => s.reason === "leased").map((s) => path.basename(s.path)), ["rk-live"]);
});

test("sweep: a stray cargo target inside a leased run root stays with the run", () => {
  const { scratch, policy } = leaseFixture();
  const now = Date.now();
  policy.protectedSegments = ["Application Support", ".rightkit-managed"];
  policy.strayTargets = { mac: [{ path: scratch, depth: 4 }] };
  policy.strayTargetDays = 1;
  for (const [name, lease] of [["rk-live", leaseOf("rk-live", now + DAY)], ["rk-free", null]]) {
    leasedRun(scratch, name, lease);
    cargoTarget(path.join(scratch, name, "repo", "target"), now - 5 * DAY);
    for (const dir of [path.join(scratch, name, "repo"), path.join(scratch, name)]) utimesSync(dir, (now - 5 * DAY) / 1000, (now - 5 * DAY) / 1000);
  }
  const plan = planSweep({ policy, platform: "darwin", now, openPaths: [], size: () => 10, scan: () => ({ unknown: [], skipped: [] }) });
  const strays = plan.candidates.filter((c) => c.writer === "stray-cargo-target").map((c) => path.relative(scratch, c.path));
  assert.deepEqual(strays, []); // rk-free's run (scratch child, idle) takes its target along as one unit instead
  assert.deepEqual(plan.candidates.map((c) => path.basename(c.path)), ["rk-free"]);
  assert.deepEqual(plan.skipped.filter((s) => s.reason === "leased").map((s) => path.relative(scratch, s.path)).sort(), ["rk-live", path.join("rk-live", "repo", "target")].sort());
});

test("readRunLease and leasedByRunRoot read only a regular, well-formed lease and compare with the injected clock", () => {
  const { scratch } = leaseFixture();
  const now = Date.parse("2026-10-24T12:00:00Z");
  leasedRun(scratch, "rk-a", leaseOf("rk-a", now + 1000));
  assert.equal(readRunLease(path.join(scratch, "rk-a")).name, "rk-a");
  assert.equal(leasedByRunRoot(path.join(scratch, "rk-a"), now), true);
  assert.equal(leasedByRunRoot(path.join(scratch, "rk-a"), now + 1000), false); // until is exclusive
  assert.equal(readRunLease(path.join(scratch, "nothing")), null);
});

test("committed policy: per-owner scratch caps, and readDiskPolicy rejects a malformed cap", () => {
  const policy = readDiskPolicy();
  assert.deepEqual(policy.scratchPrefixCaps, { legion: 40 * 1024 ** 3, rk: 30 * 1024 ** 3, pulse: 20 * 1024 ** 3, hr: 20 * 1024 ** 3 });
  const { root, policy: base } = fixture();
  const pf = path.join(root, "bad.json");
  for (const caps of [{ "a-b": 1 }, { rk: -1 }, { rk: "5" }, { "": 1 }]) {
    writeFileSync(pf, JSON.stringify({ ...base, scratchPrefixCaps: caps }));
    assert.throws(() => readDiskPolicy(pf), /scratchPrefixCaps/);
  }
});

test("right-release --help loads and lists the hourly run modes, run-root and the 26 h status rule", () => {
  const r = spawnSync(process.execPath, [fileURLToPath(new URL("./cli/right-release.mjs", import.meta.url)), "--help"], { encoding: "utf8" });
  assert.equal(r.status, 0, r.stderr);
  assert.match(r.stdout, /disk run \[--auto\|--light\]/);
  assert.match(r.stdout, /disk run-root <name> \[--days N\]/);
  assert.match(r.stdout, /disk run-root --release <name>/);
  assert.match(r.stdout, /Hourly launchd job .* running "disk run --auto".* 26 h/);
});

// ---- review findings ---------------------------------------------------------------------------------------------------
// Each test below failed before its fix.

test("run-root: --days is at most 30 (and positive), and nothing is written for a refused value", async () => {
  assert.equal(MAX_LEASE_DAYS, 30);
  const { scratch, cmd } = leaseFixture();
  for (const days of ["31", "30.5", "365", "2999", "0", "-1", "abc", "Infinity", "NaN"]) {
    const r = await cmd(["rk-days", "--days", days]);
    assert.equal(r.exitCode, 1, days);
    assert.match(r.err, /--days must be a positive number of at most 30/, days);
    assert.equal(existsSync(path.join(scratch, "rk-days")), false, `${days}: nothing was written`);
  }
  assert.equal((await cmd(["rk-days", "--days", "30"])).exitCode, 0);
  const lease = JSON.parse(readFileSync(path.join(scratch, "rk-days", RUN_LEASE_FILE), "utf8"));
  assert.equal(Date.parse(lease.leaseUntil) - Date.parse(lease.createdAt), 30 * DAY);
  assert.equal((await cmd(["rk-days2", "--days", "0.5"])).exitCode, 0);
  assert.throws(() => createRunRoot({ policy: leaseFixture().policy, platform: "darwin", name: "rk-direct", days: 31, size: () => 0 }), /at most 30/);
});

test("run-root: renewing keeps createdAt only while the lease fits 30 days from it; a longer-running run root starts a new window the guard accepts", async () => {
  const { scratch, now, cmd } = leaseFixture();
  await cmd(["rk-r", "--days", "20"]);
  const file = path.join(scratch, "rk-r", RUN_LEASE_FILE);
  const first = JSON.parse(readFileSync(file, "utf8"));
  // renewed 10 days later for 20 more: 30 days from the original createdAt, still one window
  await cmd(["rk-r", "--days", "20"], { runRoot: { size: () => 0, now: now + 10 * DAY } });
  const second = JSON.parse(readFileSync(file, "utf8"));
  assert.equal(second.createdAt, first.createdAt);
  assert.equal(Date.parse(second.leaseUntil) - Date.parse(second.createdAt), 30 * DAY);
  // renewed 25 days in for 20 more: 45 days from the original createdAt would break the limit, so createdAt moves to the renewal
  const later = now + 25 * DAY;
  await cmd(["rk-r", "--days", "20"], { runRoot: { size: () => 0, now: later } });
  const third = JSON.parse(readFileSync(file, "utf8"));
  assert.equal(third.createdAt, new Date(later).toISOString());
  assert.equal(third.leaseUntil, new Date(later + 20 * DAY).toISOString());
  assert.equal(leasedByRunRoot(path.join(scratch, "rk-r"), later + DAY), true);
});

test("a lease counts only in its own directory, within 30 days of its createdAt, created no later than tomorrow", () => {
  const { scratch } = leaseFixture();
  const now = Date.parse("2026-10-24T12:00:00Z");
  const lease = (name, createdAt, leaseUntil) => ({ schema: 1, name, createdAt, leaseUntil });
  const at = (name, value) => { leasedRun(scratch, name, value); return path.join(scratch, name); };
  const iso = (ms) => new Date(ms).toISOString();
  const good = at("rk-good", lease("rk-good", iso(now - DAY), iso(now + 29 * DAY)));
  assert.equal(leasedByRunRoot(good, now), true);
  // the forged file the review reproduced: a lease that says 2999
  const forged = at("rk-forged", lease("rk-forged", iso(now), "2999-01-01T00:00:00.000Z"));
  assert.equal(readRunLease(forged), null);
  assert.equal(leasedByRunRoot(forged, now), false);
  // 31 days from createdAt is the most that is tolerated (30 plus one), one millisecond more is not a lease
  const edge = at("rk-edge", lease("rk-edge", iso(now - DAY), iso(now + 30 * DAY)));
  assert.equal(leasedByRunRoot(edge, now), true);
  const over = at("rk-over", lease("rk-over", iso(now - DAY), iso(now + 30 * DAY + 1)));
  assert.equal(readRunLease(over), null);
  assert.equal(leasedByRunRoot(over, now), false);
  // created in the future: not a lease until a day before it begins
  const future = at("rk-future", lease("rk-future", iso(now + 5 * DAY), iso(now + 20 * DAY)));
  assert.equal(leasedByRunRoot(future, now), false);
  assert.equal(leasedByRunRoot(future, now + 4 * DAY), true);
  assert.equal(leasedByRunRoot(future, now + 4 * DAY - 1), false);
  const far = at("rk-far", lease("rk-far", "2999-01-01T00:00:00.000Z", "2999-01-20T00:00:00.000Z"));
  assert.equal(leasedByRunRoot(far, now), false);
  // a lease file copied from another run root names that run root and protects nothing here
  const copied = at("rk-copied", lease("rk-good", iso(now - DAY), iso(now + 5 * DAY)));
  assert.equal(readRunLease(copied), null);
  assert.equal(leasedByRunRoot(copied, now), false);
});

test("sweep: a forged lease (2999, a copied file, one created in the future) does not keep an idle scratch run from moving", () => {
  const { scratch, policy } = leaseFixture();
  const now = Date.now();
  const iso = (ms) => new Date(ms).toISOString();
  leasedRun(scratch, "rk-live", { schema: 1, name: "rk-live", createdAt: iso(now - DAY), leaseUntil: iso(now + DAY) });
  leasedRun(scratch, "rk-2999", { schema: 1, name: "rk-2999", createdAt: iso(now), leaseUntil: "2999-01-01T00:00:00.000Z" });
  leasedRun(scratch, "rk-copy", { schema: 1, name: "rk-live", createdAt: iso(now - DAY), leaseUntil: iso(now + DAY) });
  leasedRun(scratch, "rk-ahead", { schema: 1, name: "rk-ahead", createdAt: iso(now + 9 * DAY), leaseUntil: iso(now + 12 * DAY) });
  const plan = planSweep({ policy, platform: "darwin", now, openPaths: [], size: () => 10, scan: () => ({ unknown: [], skipped: [] }) });
  assert.deepEqual(plan.candidates.map((c) => path.basename(c.path)).sort(), ["rk-2999", "rk-ahead", "rk-copy"]);
  assert.deepEqual(plan.skipped.filter((x) => x.reason === "leased").map((x) => path.basename(x.path)), ["rk-live"]);
});

test("apply: the same name from several parents moves every time (a `target` in each checkout), in order .1, .2, and the move log names the source parent", () => {
  const { root, ws, policy, now } = strayFixture();
  for (const name of ["e", "f"]) cargoTarget(path.join(ws, name, "target"), OLD);
  policy.holding = { mac: [{ prefix: root, path: path.join(root, "hold") }] };
  const plan = planSweep({ policy, platform: "darwin", now, openPaths: [], size: () => 10, scan: () => ({ unknown: [], skipped: [] }) });
  const targets = plan.candidates.filter((c) => path.basename(c.path) === "target");
  assert.deepEqual(targets.map((c) => path.relative(ws, path.dirname(c.path))), ["a", "e", "f"]); // c is fresh, b is untagged
  const date = new Date("2026-10-15T12:00:00Z");
  const held = path.join(root, "hold", "2026-10-15", "stray-cargo-target");
  mkdirSync(path.join(held, "target.1"), { recursive: true }); // an earlier sweep's leftover takes the first free suffix away
  mkdirSync(path.join(held, "target"), { recursive: true });
  const applied = applySweep({ plan, policy, platform: "darwin", date });
  assert.deepEqual(applied.results.filter((r) => !r.ok).map((r) => r.error), []); // before: ENOTEMPTY on the third
  const moved = applied.results.filter((r) => path.basename(r.path) === "target");
  assert.deepEqual(moved.map((r) => path.relative(held, r.destination)), ["target.2", "target.3", "target.4"]);
  for (const r of moved) assert.equal(existsSync(path.join(r.destination, "CACHEDIR.TAG")), true);
  for (const name of ["a", "e", "f"]) assert.equal(existsSync(path.join(ws, name, "target")), false);
  // no clock value in a name, and the log says where each one came from
  assert.equal(moved.some((r) => /\.\d{8,}$/.test(r.destination)), false);
  const lines = readFileSync(applied.logFile, "utf8").trim().split("\n").map((l) => JSON.parse(l)).filter((e) => e.event === "disk.sweep.move" && path.basename(e.path) === "target");
  assert.deepEqual(lines.map((e) => path.relative(ws, e.sourceParent)), ["a", "e", "f"]);
});

test("apply: a candidate kept, leased or touched since the plan is left in place, with a logged reason, and the rest still moves", () => {
  const { scratch, policy } = leaseFixture();
  const now = Date.now();
  policy.holding = { mac: [{ prefix: scratch, path: path.join(path.dirname(scratch), "hold") }] };
  for (const name of ["rk-keep", "rk-lease", "rk-touch", "rk-free"]) leasedRun(scratch, name, null);
  const plan = planSweep({ policy, platform: "darwin", now, openPaths: [], size: () => 10, scan: () => ({ unknown: [], skipped: [] }) });
  assert.deepEqual(plan.candidates.map((c) => path.basename(c.path)).sort(), ["rk-free", "rk-keep", "rk-lease", "rk-touch"]);
  // after the plan: the agent drops a .keep, runs `disk run-root`, and writes a file
  writeFileSync(path.join(scratch, "rk-keep", KEEP_MARKER), "");
  writeFileSync(path.join(scratch, "rk-lease", RUN_LEASE_FILE), JSON.stringify({ schema: 1, name: "rk-lease", createdAt: new Date(now).toISOString(), leaseUntil: new Date(now + DAY).toISOString() }));
  writeFileSync(path.join(scratch, "rk-touch", "new.txt"), "x");
  for (const deleteMode of [false, true]) {
    const applied = applySweep({ plan: deleteMode ? { ...plan, candidates: plan.candidates.filter((c) => path.basename(c.path) !== "rk-free") } : plan, policy, platform: "darwin", deleteMode, date: new Date("2026-10-15T12:00:00Z") });
    assert.deepEqual(Object.fromEntries(applied.skipped.map((r) => [path.basename(r.path), r.reason])), { "rk-keep": "rechecked-keep", "rk-lease": "rechecked-leased", "rk-touch": "rechecked-fresh" });
    assert.deepEqual(applied.results.map((r) => [path.basename(r.path), r.ok]), deleteMode ? [] : [["rk-free", true]]);
    assert.equal(applied.movedBytes, deleteMode ? 0 : 10);
    for (const name of ["rk-keep", "rk-lease", "rk-touch"]) assert.equal(existsSync(path.join(scratch, name, "f")), true, name);
    const logged = readFileSync(applied.logFile, "utf8").trim().split("\n").map((l) => JSON.parse(l)).filter((e) => e.event === "disk.sweep.skip");
    assert.deepEqual(logged.slice(-3).map((e) => e.reason).sort(), ["rechecked-fresh", "rechecked-keep", "rechecked-leased"]);
  }
  assert.equal(existsSync(path.join(scratch, "rk-free")), false);
  assert.match(renderSweep(plan, { results: [], skipped: [{ path: path.join(scratch, "rk-keep"), reason: "rechecked-keep" }], movedBytes: 0 }), /left in place at apply time: rk-keep \(rechecked-keep\)/);
});

test("apply: a stray target whose run root was kept or leased since the plan stays; an expired marker or lease does not hold it", () => {
  const { scratch, policy } = leaseFixture();
  const now = Date.now();
  policy.protectedSegments = ["Application Support", ".rightkit-managed"];
  policy.strayTargets = { mac: [{ path: scratch, depth: 4 }] };
  policy.strayTargetDays = 1;
  policy.holding = { mac: [{ prefix: scratch, path: path.join(path.dirname(scratch), "hold") }] };
  // the scratch run itself is idle for 5 days; the strays are the planned units because the run is held open by something fresh
  for (const name of ["rk-k", "rk-l", "rk-x"]) {
    file(path.join(scratch, name, "work", "fresh.txt"), 10); // fresh content keeps the run itself out of the scratch pass
    cargoTarget(path.join(scratch, name, "repo", "target"), now - 5 * DAY);
    for (const dir of [path.join(scratch, name, "repo")]) utimesSync(dir, (now - 5 * DAY) / 1000, (now - 5 * DAY) / 1000);
  }
  const plan = planSweep({ policy, platform: "darwin", now, openPaths: [], size: () => 10, scan: () => ({ unknown: [], skipped: [] }) });
  const rel = (p) => path.relative(scratch, p).split(path.sep).join("/");
  assert.deepEqual(plan.candidates.map((c) => rel(c.path)).sort(), ["rk-k/repo/target", "rk-l/repo/target", "rk-x/repo/target"]);
  writeFileSync(path.join(scratch, "rk-k", KEEP_MARKER), "");
  writeFileSync(path.join(scratch, "rk-l", RUN_LEASE_FILE), JSON.stringify({ schema: 1, name: "rk-l", createdAt: new Date(now).toISOString(), leaseUntil: new Date(now + DAY).toISOString() }));
  const applied = applySweep({ plan, policy, platform: "darwin", date: new Date("2026-10-15T12:00:00Z") });
  assert.deepEqual(Object.fromEntries(applied.skipped.map((r) => [rel(r.path), r.reason])), { "rk-k/repo/target": "rechecked-keep", "rk-l/repo/target": "rechecked-leased" });
  assert.deepEqual(applied.results.map((r) => [rel(r.path), r.ok]), [["rk-x/repo/target", true]]);
  assert.equal(existsSync(path.join(scratch, "rk-k", "repo", "target")), true);
  assert.equal(existsSync(path.join(scratch, "rk-l", "repo", "target")), true);
  assert.equal(existsSync(path.join(scratch, "rk-x", "repo", "target")), false);
});

// Two volumes: `root` is the squeezed agent volume (device 1) with its holding base `holdD`; `other` stands for the internal drive or C:
// (device 2) with `holdC`. Both bases carry a long-expired dated folder, so only the purge decision decides what remains.
function twoVolumeFixture() {
  const { root, heavy, policy } = fixture();
  const other = mkdtempSync(path.join(os.tmpdir(), "rk-other-"));
  const holdD = path.join(root, "holdD");
  const holdC = path.join(other, "holdC");
  policy.floors = { mac: [{ id: "d", path: root, label: "fixture", warnFreeBytes: 1000, agentVolume: true }] };
  policy.holding = { mac: [{ prefix: root, path: holdD }, { prefix: other, path: holdC }] };
  policy.writers = [];
  const pf = path.join(root, "policy.json");
  writeFileSync(pf, JSON.stringify(policy));
  for (const base of [holdD, holdC]) { file(path.join(base, "2020-01-01", "w", "f"), 10, OLD); writeFileSync(path.join(base, HOLDING_MARKER), ""); }
  const deviceOfFake = (p) => (p.startsWith(other) ? 2 : 1);
  const run = (args, { device = deviceOfFake, freeBytes = 1 } = {}) => runDiskCommand(["run", ...args, "--policy", pf], { platform: "darwin", stdout: () => {}, deps: { selfCheck: () => ({ ok: true, problems: [] }), openPaths: [], deviceOf: device, purge: { now: Date.now() + 3600_000 }, plan: { size: () => 10 }, check: { size: () => 0, space: () => ({ freeBytes, totalBytes: 10000 }) } } });
  calibrate(heavy);
  return { root, heavy, other, holdD, holdC, run, dated: (base) => existsSync(path.join(base, "2020-01-01")) };
}

test("emergency purge empties only the holding bases on the squeezed volume, in the full pass and the light pass", async () => {
  for (const args of [[], ["--light"]]) {
    const { holdD, holdC, run, dated } = twoVolumeFixture();
    const r = await run(args);
    assert.equal(r.exitCode === 0 || r.exitCode === 3, true);
    assert.equal(dated(holdD), false, `${args}: the squeezed volume's holding is purged`);
    assert.equal(dated(holdC), true, `${args}: the other volume's holding keeps its undo window`);
  }
  // with both bases on one volume the same emergency purges both: the volume, not the base, decides
  const same = twoVolumeFixture();
  await same.run([], { device: () => 1 });
  assert.equal(same.dated(same.holdD), false);
  assert.equal(same.dated(same.holdC), false);
  // a volume whose device cannot be read purges nothing
  const unknown = twoVolumeFixture();
  await unknown.run([], { device: () => null });
  assert.equal(unknown.dated(unknown.holdD), true);
  assert.equal(unknown.dated(unknown.holdC), true);
});

test("holdingBasesOnVolume: the known bases and the legacy heavy dir on that volume's device, nothing else", () => {
  const { root, heavy, policy } = fixture();
  const other = path.join(root, "elsewhere");
  policy.holding = { mac: [{ prefix: root, path: path.join(root, "hold") }, { prefix: root, path: path.join(other, "hold") }] };
  const device = (p) => (p.startsWith(other) ? 7 : 1);
  const bases = holdingBasesOnVolume(root, { policy, platform: "darwin", deviceOf: device });
  assert.equal(bases.includes(path.join(root, "hold")), true);
  assert.equal(bases.includes(heavy), true); // the legacy heavy dir sits on the squeezed volume here
  assert.equal(bases.some((b) => b.startsWith(other)), false);
  assert.deepEqual(holdingBasesOnVolume(root, { policy, platform: "darwin", deviceOf: () => null }), []);
});

// A full log: one full-pass line, then a busy day of sweep lines (about 450 bytes each, so over 4 MiB of them).
function fillerLine(ts) {
  return `${JSON.stringify({ ts, event: "disk.sweep.move", writer: "agent-tmp", path: `/Volumes/D/agent-cache/tmp/${"x".repeat(120)}`, root: "/Volumes/D/agent-cache/tmp", bytes: 123456, mtime: "2026-10-22T00:00:00.000Z", destination: `/Volumes/D/.rightkit-swept/2026-10-24/agent-tmp/${"x".repeat(100)}`, how: "rename" })}\n`;
}
function busyLog(file, fullTs, lines = 12000) {
  writeFileSync(file, `${JSON.stringify({ ts: fullTs, event: "disk.run.ok", pass: "full" })}\n${JSON.stringify({ ts: fullTs, event: "disk.report" })}\n${fillerLine(fullTs).repeat(lines)}`);
  assert.ok(statSync(file).size > 4 * 1024 * 1024, "the filler pushes the full-pass line out of the last 4 MiB");
}

test("lastEvent scans backwards in growing windows: a full-pass line under more than 4 MiB of sweep lines is still found", () => {
  const dir = mkdtempSync(path.join(os.tmpdir(), "rk-log-"));
  const log = path.join(dir, "disk-hygiene.jsonl");
  busyLog(log, "2026-10-24T04:05:00.000Z");
  assert.equal(lastEvent(log, ["disk.run.ok", "disk.floor.warn"], { accept: (e) => e.pass !== "light" })?.ts, "2026-10-24T04:05:00.000Z"); // before: null
  assert.equal(lastEvent(log, ["disk.report"])?.event, "disk.report");
  assert.equal(lastEvent(log, ["disk.run.light"]), null); // the whole file was read and holds none
  // the windows grow only as far as needed, and the last one reaches the start of the file
  const small = path.join(dir, "small.jsonl");
  writeFileSync(small, `${JSON.stringify({ ts: "t0", event: "needle" })}\n${fillerLine("t1").repeat(300)}`);
  assert.equal(lastEvent(small, ["needle"], { windows: [1024, 4096, 1 << 20] })?.ts, "t0");
  assert.equal(lastEvent(small, ["needle"], { windows: [1024, 4096] }), null); // the last window stops short: not found, not an error
  assert.equal(lastEvent(small, ["needle"], { maxBytes: 1024 }), null);
  assert.equal(lastEvent(small, ["fillerless"], { windows: [1024, 4096, 1 << 20] }), null);
  // a newer match wins over an older one in an earlier window
  appendFileSync(small, `${JSON.stringify({ ts: "t9", event: "needle" })}\n`);
  assert.equal(lastEvent(small, ["needle"], { windows: [1024, 4096, 1 << 20] })?.ts, "t9");
});

test("lastEvent: a missing log means the job never ran (null); any other read error is thrown, never turned into 'nothing found'", () => {
  const dir = mkdtempSync(path.join(os.tmpdir(), "rk-log-"));
  assert.equal(lastEvent(path.join(dir, "absent.jsonl"), ["disk.run.ok"]), null);
  assert.throws(() => lastEvent(dir, ["disk.run.ok"]), (error) => error.code === "EISDIR"); // a directory where the log should be
  writeFileSync(path.join(dir, "plain"), "x");
  assert.throws(() => lastEvent(path.join(dir, "plain", "disk-hygiene.jsonl"), ["disk.run.ok"]), (error) => error.code === "ENOTDIR");
});

test("throughFile: a missing path is missing; an ancestor that is a file is ENOTDIR; any other ancestor error propagates (never 'never ran')", async () => {
  const { throughFile } = await import("./disk-policy.mjs");
  const dir = mkdtempSync(path.join(os.tmpdir(), "rk-through-"));
  assert.equal(throughFile(path.join(dir, "a", "b", "disk-hygiene.jsonl")), false, "missing directories below an existing one: missing");
  writeFileSync(path.join(dir, "plain"), "x");
  assert.throws(() => throughFile(path.join(dir, "plain", "disk-hygiene.jsonl")), (error) => error.code === "ENOTDIR");
  for (const code of ["EACCES", "EIO"]) {
    const stat = (p) => { if (p === dir) throw Object.assign(new Error(code), { code }); throw Object.assign(new Error("ENOENT"), { code: "ENOENT" }); };
    assert.throws(() => throughFile(path.join(dir, "gone", "disk-hygiene.jsonl"), stat), (error) => error.code === code, code);
  }
});

test("lastEvent: a directory that reports size 0 is a read error, not an empty log (Windows reports 0 for directories; /proc does here)", { skip: !existsSync("/proc/self/task") || statSync("/proc/self/task").size !== 0 }, () => {
  assert.throws(() => lastEvent("/proc/self/task", ["disk.run.ok"]), (error) => error.code === "EISDIR");
  assert.deepEqual(readRecentEvents("/proc/self/task"), [], "the lenient reader still returns an empty list");
});

test("run --auto: a full pass buried under more than 4 MiB of sweep lines still counts, so the hour stays light", async () => {
  const lastSlot = new Date(2026, 9, 24, 3, 35).toISOString();
  const now = local(24, 9, 0);
  const { heavy, run } = lightFixture();
  mkdirSync(heavy, { recursive: true });
  busyLog(path.join(heavy, "disk-hygiene.jsonl"), lastSlot);
  const r = await run(["--auto"], { now, plan: { size: () => 10 }, check: { size: () => 0, space: () => ({ freeBytes: 5000, totalBytes: 10000 }) } });
  assert.equal(r.pass, "light"); // before: lastFull null, so a full pass every hour
});

test("run --auto: a log that exists but cannot be read keeps the run light and says why; a missing log is a full pass", async () => {
  const now = local(24, 9, 0);
  const broken = lightFixture();
  mkdirSync(path.join(broken.heavy, "disk-hygiene.jsonl"), { recursive: true }); // EISDIR on read
  const out = [];
  const r = await runDiskCommand(["run", "--auto", "--policy", path.join(broken.root, "policy.json")], { platform: "darwin", stdout: (l) => out.push(l), deps: { selfCheck: () => ({ ok: true, problems: [] }), openPaths: [], now, plan: { size: () => 10, scan: () => ({ unknown: [], skipped: [] }) }, check: { size: () => 0, space: () => ({ freeBytes: 5000, totalBytes: 10000 }) }, purge: { bases: [] } } });
  assert.equal(r.pass, "light");
  assert.equal(r.exitCode, 0);
  assert.match(out.join("\n"), /WARNING: the run log .* cannot be read .*EISDIR.*stays light/);
  const missing = lightFixture(); // heavy dir exists after the first event only: no log file at all
  mkdirSync(missing.heavy, { recursive: true });
  const m = await missing.run(["--auto"], { now, plan: { size: () => 10 }, check: { size: () => 0, space: () => ({ freeBytes: 5000, totalBytes: 10000 }) } });
  assert.equal(m.pass, "full");
});

test("schedule status: more than 4 MiB of sweep lines after the last full run do not hide it; an unreadable log is reported as such", async () => {
  const busy = statusFixture([]);
  busyLog(busy.logFile, "2026-10-24T09:00:00.000Z"); // 3 h before the fixture's clock
  const ok = await busy.status();
  assert.match(ok.text, /last full run: 3\.0 h ago/);
  assert.doesNotMatch(ok.text, /PROBLEM/);
  assert.equal(ok.exitCode, 0);
  const broken = statusFixture([]);
  rmSync(broken.logFile);
  mkdirSync(broken.logFile); // a directory where the log should be
  const r = await broken.status();
  assert.equal(r.exitCode, 1);
  assert.match(r.text, /PROBLEM: the run log .* cannot be read \(EISDIR/);
  assert.doesNotMatch(r.text, /no completed full run/);
  const missing = statusFixture(null); // no log at all: the job never ran
  const none = await missing.status();
  assert.equal(none.exitCode, 1);
  assert.match(none.text, /PROBLEM: no completed full run recorded/);
});

// A full run with an internal drive listed first and the agent volume second: whichever number the events carry is the one that matters.
function leadFixture({ agentReadable = true } = {}) {
  const fx = lightFixture();
  const pf = path.join(fx.root, "policy.json");
  const policy = JSON.parse(readFileSync(pf, "utf8"));
  const internal = path.join(fx.root, "internal");
  mkdirSync(internal);
  policy.floors = { mac: [{ id: "mac-internal", path: internal, label: "Mac internal", warnFreeBytes: 1000, holdingSource: true }, { id: "mac-d", path: fx.root, label: "Mac D:", warnFreeBytes: 1000, agentVolume: true }] };
  writeFileSync(pf, JSON.stringify(policy));
  const space = (p) => {
    if (p === internal) return { freeBytes: 111, totalBytes: 500 };
    if (!agentReadable) throw new Error("ENOENT: not mounted");
    return { freeBytes: 7777, totalBytes: 10000 };
  };
  return { ...fx, run: (args) => fx.run(args, { check: { size: () => 0, space }, ...(args.includes("--light") ? {} : { plan: { size: () => 10 } }) }) };
}

test("every run event reports the agent volume's free bytes with its floor id, never the first floor's", async () => {
  const full = leadFixture();
  await full.run([]);
  const fullEvent = full.events().find((e) => e.event === "disk.run.ok" || e.event === "disk.floor.warn");
  assert.deepEqual([fullEvent.freeBytes, fullEvent.floorId], [7777, "mac-d"]); // before: 111, the internal drive
  const light = leadFixture();
  await light.run(["--light"]);
  const lightEvent = light.events().find((e) => e.event === "disk.run.light");
  assert.deepEqual([lightEvent.freeBytes, lightEvent.floorId], [7777, "mac-d"]);
  assert.deepEqual(lightEvent.floors.map((f) => f.id), ["mac-internal", "mac-d"]); // the per-floor list still names both
  // the agent volume is not mounted: null, with its id, on every event and in the report; the internal drive's 111 appears nowhere as "the" free space
  const gone = leadFixture({ agentReadable: false });
  await gone.run([]);
  const goneFull = gone.events().find((e) => e.event === "disk.run.ok" || e.event === "disk.floor.warn");
  assert.deepEqual([goneFull.freeBytes, goneFull.floorId], [null, "mac-d"]);
  const report = gone.events().find((e) => e.event === "disk.report");
  assert.deepEqual([report.freeBytes, report.usedBytes, report.categories, report.delta], [null, null, null, null]);
  const goneLight = leadFixture({ agentReadable: false });
  await goneLight.run(["--light"]);
  const goneLightEvent = goneLight.events().find((e) => e.event === "disk.run.light");
  assert.deepEqual([goneLightEvent.freeBytes, goneLightEvent.floorId], [null, "mac-d"]);
});

test("the usage line lists run [--auto|--light], run-root <name> [--days N] [--release] and schedule status", async () => {
  const { root } = fixture();
  const pf = path.join(root, "policy.json");
  writeFileSync(pf, JSON.stringify(fixture().policy));
  await assert.rejects(runDiskCommand(["nonsense", "--policy", pf], { platform: "darwin", stdout: () => {} }), (error) => {
    assert.match(error.message, /\|run \[--auto\|--light\]\|/);
    assert.match(error.message, /\|run-root <name> \[--days N\] \[--release\]\|/);
    assert.match(error.message, /\|schedule install\|uninstall\|status\|/);
    return true;
  });
});
