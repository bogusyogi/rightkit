import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { EventEmitter } from "node:events";
import { createSwapPaths, resolveSwapHelper, recoverApp, recoverInstalls, journalPathFor, installApp, matchPids, mountPointsFromAttach, parseArgs, parseTeamId, parsePlist, trashName, main } from "./install-app.mjs";

// Not run on Windows: installs a macOS app bundle and signals processes the POSIX way.
const WINDOWS_SKIP = process.platform === "win32" ? "installs a macOS app bundle and signals processes the POSIX way" : false;

const PLIST = `<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict><key>system-entities</key><array>
<dict><key>content-hint</key><string>GUID_partition_scheme</string><key>dev-entry</key><string>/dev/disk4</string></dict>
<dict><key>dev-entry</key><string>/dev/disk4s1</string><key>mount-point</key><string>/tmp/a &amp; b/Pulse</string><key>x</key><true/><key>n</key><integer>3</integer></dict>
</array></dict></plist>`;

test("parsePlist and mountPointsFromAttach", () => {
  assert.deepEqual(mountPointsFromAttach(PLIST), ["/tmp/a & b/Pulse"]);
  const p = parsePlist(PLIST);
  assert.equal(p["system-entities"][1].n, 3);
  assert.equal(p["system-entities"][1].x, true);
  assert.throws(() => mountPointsFromAttach("<plist><dict></dict></plist>"), /system-entities/);
});

test("parseTeamId", () => {
  assert.equal(parseTeamId("Identifier=x\nTeamIdentifier=ABCDE12345\n"), "ABCDE12345");
  assert.equal(parseTeamId("TeamIdentifier=not set"), null);
});

test("matchPids only matches executables inside the bundle MacOS dir", () => {
  const ps = [
    "  101 /Applications/Pulse.app/Contents/MacOS/Pulse",
    "  102 /Applications/Pulse.app/Contents/Frameworks/Helper.app/Contents/MacOS/Helper",
    "  103 /Applications/Pulse.app.bak/Contents/MacOS/Pulse",
    "  104 /Applications/Pulse.app/Contents/MacOS/Pulse Helper",
    "    1 /sbin/launchd",
  ].join("\n");
  assert.deepEqual(matchPids(ps, "/Applications/Pulse.app"), [101, 104]);
});

test("parseArgs validation", () => {
  assert.throws(() => parseArgs([]), /--dmg/);
  assert.throws(() => parseArgs(["--dmg", "x.zip"]), /\.dmg/);
  assert.throws(() => parseArgs(["--dmg", "x.dmg", "--bogus"]), /unknown/);
  assert.throws(() => parseArgs(["--dmg", "x.dmg", "--app-name", "../x.app"]), /app-name/);
  assert.throws(() => parseArgs(["--dmg", "x.dmg", "--bundle-id", "a b"]), /bundle/);
  assert.throws(() => parseArgs(["--dmg", "x.dmg", "--relaunch", "--no-relaunch"]), /exclusive/);
  assert.throws(() => parseArgs(["--dmg"]), /requires a value/);
  const o = parseArgs(["--dmg", "x.dmg", "--json", "--no-relaunch"]);
  assert.equal(o.dest, "/Applications");
  assert.equal(o.relaunch, false);
  assert.equal(o.json, true);
});

test("trashName", () => {
  assert.equal(trashName("Pulse.app", new Date("2026-10-09T01:02:03.456Z")), "Pulse 2026-10-09-01-02-03.app");
});

test("win32 is rejected", async () => {
  const lines = [];
  const code = await main(["--dmg", "x.dmg", "--json"], { stdout: (s) => lines.push(s), stderr: () => {}, deps: { platform: "win32" } });
  assert.equal(code, 3);
  assert.match(lines.join("\n"), /not implemented on win32/);
});

// ---- orchestration with stubs ----

function harness({ team = "TEAM1", installedTeam = "TEAM1", fail = {}, running = false, bundleId = "dev.orthic.pulse" } = {}) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "install-app-test-"));
  const dest = path.join(root, "Applications");
  const home = path.join(root, "home");
  const mount = path.join(root, "mnt");
  fs.mkdirSync(path.join(dest, "Pulse.app", "Contents"), { recursive: true });
  fs.writeFileSync(path.join(dest, "Pulse.app", "Contents", "marker"), "old");
  fs.mkdirSync(path.join(mount, "Pulse.app", "Contents"), { recursive: true });
  fs.writeFileSync(path.join(mount, "Pulse.app", "Contents", "marker"), "new");
  fs.writeFileSync(path.join(root, "p.dmg"), "");
  fs.writeFileSync(path.join(root, "p.zip"), "");
  const calls = [];
  const kills = [];
  let alive = running;
  const isNew = (p) => !p.startsWith(dest) || path.basename(p).startsWith(".");
  const execFile = async (cmd, args) => {
    calls.push([cmd, ...args]);
    const key = path.basename(cmd) + ":" + (args[0] === "stapler" ? "stapler" : args[0]);
    const fx = (name) => { if (fail[name]) { const e = new Error(fail[name]); e.stderr = fail[name]; throw e; } };
    if (cmd.endsWith("hdiutil") && args[0] === "attach") return { stdout: `<plist><dict><key>system-entities</key><array><dict><key>mount-point</key><string>${mount}</string></dict></array></dict></plist>`, stderr: "" };
    if (cmd.endsWith("hdiutil")) return { stdout: "", stderr: "" };
    if (cmd.endsWith("codesign") && args[0] === "--verify") { fx(isNew(args[3]) ? "verify" : "reverify"); return { stdout: "", stderr: "" }; }
    if (cmd.endsWith("codesign") && args[0] === "-dvv") return { stdout: "", stderr: `TeamIdentifier=${isNew(args[1]) ? team : installedTeam}\n` };
    if (cmd.endsWith("codesign")) return { stdout: "", stderr: "" };
    if (cmd.endsWith("spctl")) {
      if (args.includes("context:primary-signature")) { fx("spctl-dmg"); return { stdout: "", stderr: "" }; }
      if (args.includes("-vv")) { fx("spctl-notarized"); return { stdout: "", stderr: fail.notNotarized ? "accepted\nsource=Developer ID" : "accepted\nsource=Notarized Developer ID" }; }
      fx("spctl"); return { stdout: "", stderr: "" };
    }
    if (cmd.endsWith("xcrun") && args[0] === "--find") { fx("nostapler"); return { stdout: "/usr/bin/stapler", stderr: "" }; }
    if (cmd.endsWith("xcrun")) { if (args[2].endsWith(".app")) fx("stapler-app"); else fx("stapler-dmg"); fx("stapler"); return { stdout: "", stderr: "" }; }
    if (cmd.endsWith("plutil")) return { stdout: JSON.stringify({ CFBundleIdentifier: bundleId }), stderr: "" };
    if (cmd.endsWith("ps")) return { stdout: alive ? `  555 ${dest}/Pulse.app/Contents/MacOS/Pulse\n` : "", stderr: "" };
    if (cmd.endsWith("xattr")) { fx("xattr"); return { stdout: "", stderr: "" }; }
    if (cmd.endsWith("ditto") && args[0] === "-x") {
      fx("unzip");
      fs.cpSync(path.join(mount, "Pulse.app"), path.join(args[3], "Pulse.app"), { recursive: true });
      return { stdout: "", stderr: "" };
    }
    if (cmd.endsWith("ditto")) {
      fx("ditto");
      fs.cpSync(args[0], args[1], { recursive: true });
      return { stdout: "", stderr: "" };
    }
    if (cmd.endsWith("open")) return { stdout: "", stderr: "" };
    throw new Error(`unexpected ${cmd}`);
  };
  const events = [];
  // Stand-in for rightkit-swap: exchanges two paths (renames preserve inodes, like RENAME_SWAP).
  const fakeSwap = (a, b) => { const t = `${a}.x`; fs.renameSync(a, t); fs.renameSync(b, a); fs.renameSync(t, b); };
  // Default: no swap helper, so the two-rename path runs. Atomic tests pass deps.swapPaths = fakeSwap.
  const noHelper = () => { const e = new Error("rightkit-swap helper not found"); e.code = "ENOHELPER"; throw e; };
  const deps = { execFile, swapPaths: noHelper, platform: "darwin", home, tmpdir: root, emit: (e) => events.push(e), sleep: async () => {}, kill: (pid, sig) => { kills.push([pid, sig]); if (sig === "SIGTERM" && !fail.stubborn) alive = false; if (sig === "SIGKILL") alive = false; } };
  const opts = { dmg: path.join(root, "p.dmg"), appName: null, bundleId: null, dest, relaunch: null, json: true };
  return { root, mount, dest, home, deps, fakeSwap, opts, calls, kills, events, marker: () => fs.readFileSync(path.join(dest, "Pulse.app", "Contents", "marker"), "utf8") };
}

test("happy path replaces, trashes old, quits with SIGTERM, relaunches hidden because it was running", { skip: WINDOWS_SKIP }, async () => {
  const h = harness({ running: true });
  const r = await installApp(h.opts, h.deps);
  assert.equal(r.ok, true);
  assert.equal(h.marker(), "new");
  assert.deepEqual(h.kills, [[555, "SIGTERM"]]);
  assert.equal(fs.readFileSync(path.join(r.previous, "Contents", "marker"), "utf8"), "old");
  assert.deepEqual(h.events.map((e) => e.event), ["start", "verified", "quit", "swap-fallback", "replaced", "relaunched", "done"]);
  const open = h.calls.find((c) => c[0] === "/usr/bin/open");
  assert.deepEqual(open, ["/usr/bin/open", "-g", "-j", "-b", "dev.orthic.pulse"]);
  assert.ok(h.calls.some((c) => c[0].endsWith("hdiutil") && c[1] === "detach"));
  assert.ok(!h.calls.some((c) => /osascript/.test(c[0])));
});

test("not running and --no-relaunch: no open call; SIGKILL after stubborn process", { skip: WINDOWS_SKIP }, async () => {
  const h = harness();
  h.opts.relaunch = false;
  await installApp(h.opts, h.deps);
  assert.ok(!h.calls.some((c) => c[0] === "/usr/bin/open"));
  const s = harness({ running: true, fail: { stubborn: "x" } });
  s.opts.relaunch = false;
  await installApp(s.opts, s.deps);
  assert.deepEqual(s.kills, [[555, "SIGTERM"], [555, "SIGKILL"]]);
});

test("signature failure leaves installed app untouched and detaches", async () => {
  const h = harness({ running: true, fail: { verify: "invalid signature" } });
  await assert.rejects(installApp(h.opts, h.deps), /codesign-verify/);
  assert.equal(h.marker(), "old");
  assert.deepEqual(h.kills, []);
  assert.ok(h.calls.some((c) => c[0].endsWith("hdiutil") && c[1] === "detach"));
  assert.equal(h.events.at(-1).event, "failed");
});

test("spctl and stapler failures refuse before touching installed app", async () => {
  for (const name of ["spctl", "stapler"]) {
    const h = harness({ fail: { [name]: "rejected" } });
    await assert.rejects(installApp(h.opts, h.deps));
    assert.equal(h.marker(), "old");
  }
});

test("team-ID change is refused", async () => {
  const h = harness({ team: "NEWTEAM", installedTeam: "OLDTEAM", running: true });
  await assert.rejects(installApp(h.opts, h.deps), /team change/);
  assert.equal(h.marker(), "old");
  assert.deepEqual(h.kills, []);
});

test("bundle id mismatch with --bundle-id is refused", async () => {
  const h = harness();
  h.opts.bundleId = "dev.other.app";
  await assert.rejects(installApp(h.opts, h.deps), /bundle id/);
  assert.equal(h.marker(), "old");
});

test("ditto failure (during staging) leaves the old bundle untouched and no leftovers", async () => {
  const h = harness({ fail: { ditto: "disk full" } });
  await assert.rejects(installApp(h.opts, h.deps), /previous app untouched/);
  assert.equal(h.marker(), "old");
  assert.deepEqual(fs.readdirSync(h.dest), ["Pulse.app"]);
});

test("fresh install into empty dest works without trash", async () => {
  const h = harness();
  fs.rmSync(path.join(h.dest, "Pulse.app"), { recursive: true });
  const r = await installApp(h.opts, h.deps);
  assert.equal(r.previous, null);
  assert.equal(h.marker(), "new");
});

test("detach retries busy volumes with backoff, then forces", async () => {
  const { detachWithRetry } = await import("./hdiutil-detach.mjs");
  const calls = [], sleeps = [], logs = [];
  const outcome = await detachWithRetry({ mount: "/m", sleep: async (ms) => { sleeps.push(ms); }, log: (e) => logs.push(e),
    detach: async (args) => { calls.push(args); if (!args.includes("-force")) throw new Error("Resource busy"); } });
  assert.equal(calls.length, 6); assert.deepEqual(calls.at(-1), ["detach", "-force", "/m"]);
  assert.deepEqual(sleeps, [500, 1000, 2000, 4000]); assert.equal(outcome.forced, true); assert.equal(logs.filter((e) => e.event === "hdiutil-detach-retry").length, 5);
});

test("parseArgs --dev accepts dmg, zip, or app, exclusively; zip/app need --dev", () => {
  assert.equal(parseArgs(["--dev", "--zip", "a.zip"]).dev, true);
  assert.ok(path.isAbsolute(parseArgs(["--dev", "--zip", "a.zip"]).zip));
  assert.ok(path.isAbsolute(parseArgs(["--dev", "--app", "x/Pulse.app"]).app));
  assert.equal(parseArgs(["--dev", "--dmg", "a.dmg"]).dmg.endsWith("a.dmg"), true);
  assert.throws(() => parseArgs(["--zip", "a.zip"]), /require --dev/);
  assert.throws(() => parseArgs(["--app", "Pulse.app"]), /require --dev/);
  assert.throws(() => parseArgs(["--dev"]), /required/);
  assert.throws(() => parseArgs(["--dev", "--zip", "a.zip", "--dmg", "a.dmg"]), /mutually exclusive/);
  assert.throws(() => parseArgs(["--dev", "--zip", "a.dmg"]), /\.zip/);
  assert.throws(() => parseArgs(["--dev", "--app", "Pulse"]), /\.app/);
  assert.equal(parseArgs(["--dmg", "a.dmg"]).dev, false);
});

test("--dev zip install: skips spctl/stapler, clears quarantine on the new bundle only, flags dev", { skip: WINDOWS_SKIP }, async () => {
  const h = harness({ running: true });
  h.opts = { ...h.opts, dmg: null, zip: path.join(h.root, "p.zip"), dev: true };
  const r = await installApp(h.opts, h.deps);
  assert.equal(r.ok, true);
  assert.equal(r.dev, true);
  assert.equal(h.marker(), "new");
  assert.equal(r.previous, null, "a dev install keeps no old copy");
  assert.deepEqual(fs.readdirSync(h.dest), ["Pulse.app"], "no .previous, no staging, no journal beside the dev install");
  assert.ok(!h.calls.some((c) => c[0].endsWith("spctl")));
  assert.ok(!h.calls.some((c) => c[0].endsWith("xcrun")));
  assert.ok(!h.calls.some((c) => c[0].endsWith("hdiutil")), "zip input never mounts");
  assert.ok(h.calls.some((c) => c[0].endsWith("codesign") && c[1] === "--verify" && c.includes("--deep") && c.includes("--strict")));
  const xattrs = h.calls.filter((c) => c[0].endsWith("xattr"));
  assert.deepEqual(xattrs, [["/usr/bin/xattr", "-dr", "com.apple.quarantine", path.join(h.dest, "Pulse.app")]]);
  assert.ok(h.events.every((e) => e.dev === true));
  assert.ok(h.events.some((e) => e.event === "dev-build" && /DEV BUILD \(not notarized\)/.test(e.note)));
  assert.deepEqual(h.kills, [[555, "SIGTERM"]]);
  assert.ok(h.calls.some((c) => c[0] === "/usr/bin/open" && c.join(" ").includes("-g -j")));
  assert.deepEqual(fs.readdirSync(h.root).filter((n) => n.startsWith("right-install-app-zip-")), [], "extract dir cleaned up");
});

test("--dev app install uses the bundle in place and still enforces team continuity and bundle id", async () => {
  const h = harness();
  h.opts = { ...h.opts, dmg: null, app: path.join(h.mount, "Pulse.app"), dev: true };
  const r = await installApp(h.opts, h.deps);
  assert.equal(r.dev, true);
  assert.equal(h.marker(), "new");

  const t = harness({ team: "NEWTEAM", installedTeam: "OLDTEAM" });
  t.opts = { ...t.opts, dmg: null, app: path.join(t.mount, "Pulse.app"), dev: true };
  await assert.rejects(installApp(t.opts, t.deps), /team change/);
  assert.equal(t.marker(), "old");

  const b = harness();
  b.opts = { ...b.opts, dmg: null, app: path.join(b.mount, "Pulse.app"), dev: true, bundleId: "dev.other.app" };
  await assert.rejects(installApp(b.opts, b.deps), /bundle id/);
  assert.equal(b.marker(), "old");
});

test("--dev still refuses an invalid signature, and non-dev never touches xattr", async () => {
  const h = harness({ fail: { verify: "invalid signature" } });
  h.opts = { ...h.opts, dmg: null, zip: path.join(h.root, "p.zip"), dev: true };
  await assert.rejects(installApp(h.opts, h.deps), /codesign-verify/);
  assert.equal(h.marker(), "old");
  assert.deepEqual(fs.readdirSync(h.root).filter((n) => n.startsWith("right-install-app-zip-")), []);

  const n = harness();
  await installApp(n.opts, n.deps);
  assert.ok(!n.calls.some((c) => c[0].endsWith("xattr")));
  assert.ok(n.events.every((e) => e.dev === undefined));
});

test("--dev prints the DEV BUILD line in human mode", async () => {
  const h = harness();
  const out = [];
  const code = await main(["--dev", "--zip", path.join(h.root, "p.zip"), "--dest", h.dest], { stdout: (s) => out.push(s), stderr: () => {}, deps: h.deps });
  assert.equal(code, 0);
  assert.ok(out.includes("install-app: DEV BUILD (not notarized)"));
});

test("app ticket present: verified records app-ticket and skips DMG fallback checks", async () => {
  const h = harness();
  await installApp(h.opts, h.deps);
  assert.equal(h.events.find((e) => e.event === "verified").ticket, "app-ticket");
  assert.ok(!h.calls.some((c) => c.includes("context:primary-signature")));
});

test("DMG-ticket flow: app has no ticket, DMG staple + DMG spctl + app Notarized Developer ID accept", async () => {
  const h = harness({ fail: { "stapler-app": "Pulse.app does not have a ticket stapled" } });
  const r = await installApp(h.opts, h.deps);
  assert.equal(r.ok, true);
  assert.equal(h.marker(), "new");
  assert.equal(h.events.find((e) => e.event === "verified").ticket, "dmg-ticket");
  assert.ok(h.calls.some((c) => c[0].endsWith("xcrun") && c[3] === h.opts.dmg));
  assert.ok(h.calls.some((c) => c[0].endsWith("spctl") && c.join(" ").includes(`-a -t open --context context:primary-signature ${h.opts.dmg}`)));
  assert.ok(h.calls.some((c) => c[0].endsWith("spctl") && c.slice(1, 5).join(" ") === "-a -t exec -vv"));
});

for (const [name, fail, code] of [
  ["DMG stapler fails", { "stapler-app": "x", "stapler-dmg": "no ticket" }, /stapler-validate/],
  ["DMG spctl rejects", { "stapler-app": "x", "spctl-dmg": "rejected" }, /spctl-dmg/],
  ["app not Notarized Developer ID", { "stapler-app": "x", notNotarized: true }, /Notarized Developer ID/],
  ["app spctl rejects", { "stapler-app": "x", "spctl-notarized": "rejected" }, /spctl-notarized/],
]) {
  test(`DMG-ticket fallback refuses when ${name}, leaving installed app untouched`, async () => {
    const h = harness({ fail });
    await assert.rejects(installApp(h.opts, h.deps), code);
    assert.equal(h.marker(), "old");
  });
}

test("DMG-ticket fallback keeps team continuity check", async () => {
  const h = harness({ team: "TEAM2", fail: { "stapler-app": "x" } });
  await assert.rejects(installApp(h.opts, h.deps), /team/);
  assert.equal(h.marker(), "old");
});

// ---- crash-safe swap ----

class Crash extends Error {}

// Simulates kill -9: once the crash point fires, every fs call and runner call throws, so no cleanup can run.
function crashing(h, { at = () => null, swapAt = null }) {
  let dead = false;
  const renames = [];
  const guard = (fn) => (...a) => { if (dead) throw new Crash("dead"); return fn(...a); };
  const fsImpl = new Proxy(fs, { get(target, prop) {
    const v = target[prop];
    if (typeof v !== "function") return v;
    if (prop === "renameSync") return (from, to) => {
      if (dead) throw new Crash("dead");
      renames.push(path.basename(to));
      const hit = at(from, to, renames);
      if (hit === "before") { dead = true; throw new Crash("kill before rename"); }
      v.call(target, from, to);
      if (hit === "after") { dead = true; throw new Crash("kill after rename"); }
    };
    return guard(v.bind(target));
  } });
  const execFile = (...a) => { if (dead) throw new Crash("dead"); return h.deps.execFile(...a); };
  const swapPaths = (a, b) => {
    if (dead) throw new Crash("dead");
    if (swapAt === "before") { dead = true; throw new Crash("kill before swap"); }
    h.deps.swapPaths(a, b);
    if (swapAt === "after") { dead = true; throw new Crash("kill after swap"); }
  };
  return { fsImpl, deps: { ...h.deps, fsImpl, execFile, swapPaths, signalTarget: new EventEmitter(), exit: () => {} } };
}

const bundles = (h) => fs.readdirSync(h.dest).filter((n) => n.endsWith(".app"));
const recoverDeps = (h) => ({ dest: h.dest, appName: "Pulse.app", run: (c, a) => h.deps.execFile(c, a), fsImpl: fs, home: h.home, now: () => new Date("2026-10-09T00:00:00Z"), event: (n, d) => h.events.push({ event: n, ...d }), sleep: async () => {}, isAlive: () => false });

for (const [name, at, expectMarker] of [
  ["after backup rename, before staged rename", (from, to) => (to.endsWith(".app.previous") ? "after" : null), "old"],
  ["after staged rename, before trash", (from, to) => (to.endsWith("/Pulse.app") && from.includes(".rightkit-install-") ? "after" : null), "new"],
]) {
  test(`kill ${name}: recovery leaves exactly one valid bundle, idempotently`, { skip: WINDOWS_SKIP }, async () => {
    const h = harness();
    const c = crashing(h, { at });
    await assert.rejects(installApp(h.opts, c.deps), /kill after rename/);
    assert.ok(fs.existsSync(journalPathFor(h.dest, "Pulse.app")), "journal survives the kill");
    const r1 = await recoverInstalls(recoverDeps(h));
    assert.equal(r1.length, 1);
    assert.deepEqual(bundles(h), ["Pulse.app"]);
    assert.equal(h.marker(), expectMarker);
    assert.deepEqual(fs.readdirSync(h.dest).filter((n) => n.startsWith(".")), [], "no journal, staging, or backup left");
    assert.deepEqual(await recoverInstalls(recoverDeps(h)), [], "second recovery is a no-op");
    assert.equal(h.marker(), expectMarker);
    assert.ok(h.calls.some((x) => x[0].endsWith("hdiutil") && x[1] === "detach" && x[2] === h.mount), "stale DMG mount detached");
  });
}

test("kill after staged rename with a bad new bundle rolls back to the backup", { skip: WINDOWS_SKIP }, async () => {
  const h = harness();
  const c = crashing(h, { at: (from, to) => (to.endsWith("/Pulse.app") && from.includes(".rightkit-install-") ? "after" : null) });
  await assert.rejects(installApp(h.opts, c.deps), /kill after rename/);
  h.deps.execFile = ((orig) => async (cmd, args) => { if (cmd.endsWith("codesign") && args[0] === "--verify") throw new Error("broken"); return orig(cmd, args); })(h.deps.execFile);
  const r = await recoverInstalls(recoverDeps(h));
  assert.equal(r[0].action, "rolled-back");
  assert.equal(h.marker(), "old");
  assert.deepEqual(bundles(h), ["Pulse.app"]);
});

test("kill after backup rename with the destination absent restores the backup (the R7 finding)", async () => {
  const h = harness();
  const c = crashing(h, { at: (from, to) => (to.endsWith(".app.previous") ? "after" : null) });
  await assert.rejects(installApp(h.opts, c.deps), /kill after rename/);
  assert.ok(!fs.existsSync(path.join(h.dest, "Pulse.app")));
  const code = await main(["--recover", "--dest", h.dest, "--app-name", "Pulse.app", "--json"], { stdout: () => {}, stderr: () => {}, deps: { ...h.deps, fsImpl: fs } });
  assert.equal(code, 0);
  assert.equal(h.marker(), "old");
});

test("running install recovers a prior interrupted install first, then installs", async () => {
  const h = harness();
  const c = crashing(h, { at: (from, to) => (to.endsWith(".app.previous") ? "after" : null) });
  await assert.rejects(installApp(h.opts, c.deps), /kill after rename/);
  const r = await installApp(h.opts, { ...h.deps, isAlive: () => false });
  assert.equal(r.ok, true);
  assert.equal(h.marker(), "new");
  assert.ok(h.events.some((e) => e.event === "recovered" && e.action === "restored-backup"));
  assert.deepEqual(fs.readdirSync(h.dest).filter((n) => n.startsWith(".")), []);
});

test("recovery skips a journal whose installer is still alive", async () => {
  const h = harness();
  const c = crashing(h, { at: (from, to) => (to.endsWith(".app.previous") ? "after" : null) });
  await assert.rejects(installApp(h.opts, c.deps), /kill after rename/);
  const j = JSON.parse(fs.readFileSync(journalPathFor(h.dest, "Pulse.app"), "utf8"));
  const r = await recoverInstalls({ ...recoverDeps(h), isAlive: () => true, selfPid: j.pid + 1 });
  assert.deepEqual(r, []);
  assert.ok(fs.existsSync(journalPathFor(h.dest, "Pulse.app")));
});

test("reverify failure rolls back to the old bundle without trashing it; mount detached", async () => {
  const h = harness({ fail: { reverify: "bad after swap" } });
  await assert.rejects(installApp(h.opts, h.deps), /codesign-verify/);
  assert.equal(h.marker(), "old");
  assert.deepEqual(fs.readdirSync(h.dest).filter((n) => n.startsWith(".")), []);
  assert.ok(!fs.existsSync(path.join(h.home, ".Trash")) || fs.readdirSync(path.join(h.home, ".Trash")).length === 0);
  assert.ok(h.calls.some((x) => x[0].endsWith("hdiutil") && x[1] === "detach"));
});

test("dev: SIGTERM mid-install removes the new copy, keeps NO old copy, detaches, and exits 143", async () => {
  const h = harness();
  const sig = new EventEmitter();
  let exitCode = null;
  const exited = new Promise((resolve) => { h.deps.exit = (c) => { exitCode = c; resolve(); }; });
  const inner = h.deps.execFile;
  // Signal arrives right after the old bundle was swapped out (during quit's ps call).
  h.deps.execFile = async (cmd, args) => {
    if (cmd.endsWith("xattr") || (cmd.endsWith("codesign") && args[0] === "--verify" && args[3] === path.join(h.dest, "Pulse.app"))) { sig.emit("SIGTERM"); await exited; }
    return inner(cmd, args);
  };
  h.deps.signalTarget = sig;
  h.opts.dev = true; h.opts.dmg = null; h.opts.app = path.join(h.mount, "Pulse.app");
  const p = installApp(h.opts, h.deps).catch(() => {});
  await exited;
  assert.equal(exitCode, 143);
  assert.deepEqual(fs.readdirSync(h.dest), [], "the path is absent: no new copy, no .previous, no staging, no old copy");
  await p;
  assert.equal(sig.listenerCount("SIGTERM"), 0, "handlers removed");
});

test("SIGINT during DMG staging detaches the mount before exit", async () => {
  const h = harness();
  const sig = new EventEmitter();
  let exitCode = null;
  const exited = new Promise((resolve) => { h.deps.exit = (c) => { exitCode = c; resolve(); }; });
  const inner = h.deps.execFile;
  h.deps.execFile = async (cmd, args) => {
    if (cmd.endsWith("ditto")) { sig.emit("SIGINT"); await exited; }
    return inner(cmd, args);
  };
  h.deps.signalTarget = sig;
  installApp(h.opts, h.deps).catch(() => {});
  await exited;
  assert.equal(exitCode, 130);
  assert.ok(h.calls.some((x) => x[0].endsWith("hdiutil") && x[1] === "detach"));
  assert.equal(h.marker(), "old");
});

test("normal path leaves no journal or staging, and keeps the old bundle as Pulse.app.previous (never the Trash)", async () => {
  const h = harness();
  const r = await installApp(h.opts, h.deps);
  assert.deepEqual(fs.readdirSync(h.dest).sort(), ["Pulse.app", "Pulse.app.previous"]);
  assert.equal(r.previous, path.join(h.dest, "Pulse.app.previous"));
  assert.ok(!fs.existsSync(path.join(h.home, ".Trash")) || fs.readdirSync(path.join(h.home, ".Trash")).length === 0);
  // A second install replaces the single .previous in place.
  assert.equal(fs.readFileSync(path.join(r.previous, "Contents", "marker"), "utf8"), "old");
  await installApp(h.opts, h.deps);
  assert.deepEqual(fs.readdirSync(h.dest).sort(), ["Pulse.app", "Pulse.app.previous"]);
  // The single .previous now holds the bundle the second install replaced (the first install's new bundle).
  assert.equal(fs.readFileSync(path.join(r.previous, "Contents", "marker"), "utf8"), "new");
});

test("parseArgs --recover", () => {
  const o = parseArgs(["--recover", "--dest", "/x", "--app-name", "Pulse.app"]);
  assert.equal(o.recover, true);
  assert.throws(() => parseArgs(["--recover", "--dmg", "a.dmg"]), /--recover only/);
  assert.throws(() => parseArgs(["--recover", "--app-name", "../x.app"]), /app-name/);
});

test("no xcrun stapler: spctl-only ticket from Notarized Developer ID (+ DMG primary-signature)", async () => {
  const h = harness({ fail: { nostapler: "xcrun: error: unable to find utility" } });
  await installApp(h.opts, h.deps);
  assert.equal(h.marker(), "new");
  assert.equal(h.events.find((e) => e.event === "verified").ticket, "spctl-only");
  assert.ok(!h.calls.some((c) => c[0].endsWith("xcrun") && c[1] === "stapler"));
  assert.ok(h.calls.some((c) => c[0].endsWith("spctl") && c.includes("context:primary-signature")));
  assert.ok(h.calls.some((c) => c[0].endsWith("spctl") && c.includes("-vv")));
});

test("no xcrun stapler: refuses when the app is not Notarized Developer ID, or the DMG is rejected", async () => {
  for (const extra of [{ notNotarized: true }, { "spctl-dmg": "rejected" }]) {
    const h = harness({ fail: { nostapler: "x", ...extra } });
    await assert.rejects(installApp(h.opts, h.deps), /spctl-/);
    assert.equal(h.marker(), "old");
  }
});

test("no xcrun stapler: --dev never probes xcrun", async () => {
  const h = harness({ fail: { nostapler: "x" } });
  h.opts = { ...h.opts, dmg: null, zip: path.join(h.root, "p.zip"), dev: true };
  const r = await installApp(h.opts, h.deps);
  assert.equal(r.ok, true);
  assert.ok(!h.calls.some((c) => c[0].endsWith("xcrun")));
});

// ---- atomic swap (rightkit-swap / RENAME_SWAP) ----

const dotfiles = (h) => fs.readdirSync(h.dest).filter((n) => n.startsWith("."));

test("atomic swap: installed is never absent, old bundle trashed last, event records swap atomic", async () => {
  const h = harness();
  const swaps = [];
  let seenAbsent = false;
  h.deps.swapPaths = (a, b) => { swaps.push([path.basename(a), path.basename(b)]); h.fakeSwap(a, b); seenAbsent ||= !fs.existsSync(path.join(h.dest, "Pulse.app")); };
  const renames = [];
  h.deps.fsImpl = new Proxy(fs, { get(t, p) { const v = t[p]; if (p === "renameSync") return (f, to) => { renames.push(path.basename(to)); return v.call(t, f, to); }; return typeof v === "function" ? v.bind(t) : v; } });
  const r = await installApp(h.opts, h.deps);
  assert.equal(r.swap, "atomic");
  assert.equal(swaps.length, 1);
  assert.match(swaps[0][0], /^\.Pulse\.rightkit-install-\d+\.app$/);
  assert.equal(swaps[0][1], "Pulse.app");
  assert.ok(!renames.some((n) => n.includes("rightkit-backup")), "no backup rename in atomic mode");
  assert.equal(h.marker(), "new");
  assert.equal(fs.readFileSync(path.join(r.previous, "Contents", "marker"), "utf8"), "old");
  assert.equal(h.events.find((e) => e.event === "replaced").swap, "atomic");
  assert.deepEqual(fs.readdirSync(h.dest).sort(), ["Pulse.app", "Pulse.app.previous"]);
  assert.deepEqual(h.events.map((e) => e.event).filter((n) => n === "swap-fallback"), []);
});

for (const code of ["ENOTSUP", "EINVAL", "ENOHELPER"]) {
  test(`${code} from the helper falls back to two-rename and records it`, async () => {
    const h = harness();
    h.deps.swapPaths = () => { const e = new Error("x"); e.code = code; throw e; };
    const r = await installApp(h.opts, h.deps);
    assert.equal(r.swap, "two-rename");
    assert.equal(h.events.find((e) => e.event === "replaced").swap, "two-rename");
    assert.equal(h.events.find((e) => e.event === "swap-fallback").reason, code);
    assert.equal(h.marker(), "new");
    assert.equal(fs.readFileSync(path.join(r.previous, "Contents", "marker"), "utf8"), "old");
    assert.deepEqual(fs.readdirSync(h.dest).sort(), ["Pulse.app", "Pulse.app.previous"]);
  });
}

test("a real swap failure (not unsupported) aborts with the old bundle untouched and no leftovers", async () => {
  const h = harness();
  h.deps.swapPaths = () => { const e = new Error("EIO boom"); e.code = "EIO"; throw e; };
  await assert.rejects(installApp(h.opts, h.deps), /atomic swap failed/);
  assert.equal(h.marker(), "old");
  assert.deepEqual(dotfiles(h), []);
});

test("fresh install with the helper present uses a plain rename and reports swap rename", async () => {
  const h = harness();
  fs.rmSync(path.join(h.dest, "Pulse.app"), { recursive: true });
  h.deps.swapPaths = () => { throw new Error("must not swap when nothing is installed"); };
  const r = await installApp(h.opts, h.deps);
  assert.equal(r.swap, "rename");
  assert.equal(r.previous, null);
  assert.equal(h.marker(), "new");
});

test("atomic: re-verify failure swaps back, never trashes the old bundle", async () => {
  const h = harness({ fail: { reverify: "bad after swap" } });
  const swaps = [];
  h.deps.swapPaths = (a, b) => { swaps.push(path.basename(a)); h.fakeSwap(a, b); };
  await assert.rejects(installApp(h.opts, h.deps), /codesign-verify/);
  assert.equal(swaps.length, 2, "swap, then swap back");
  assert.equal(h.marker(), "old");
  assert.deepEqual(dotfiles(h), []);
  assert.ok(!fs.existsSync(path.join(h.home, ".Trash")) || fs.readdirSync(path.join(h.home, ".Trash")).length === 0);
});

test("atomic: swap-back falls back to renames if the helper vanishes mid-install", async () => {
  const h = harness({ fail: { reverify: "bad after swap" } });
  let calls = 0;
  h.deps.swapPaths = (a, b) => { calls += 1; if (calls > 1) { const e = new Error("gone"); e.code = "ENOHELPER"; throw e; } h.fakeSwap(a, b); };
  await assert.rejects(installApp(h.opts, h.deps), /codesign-verify/);
  assert.equal(h.marker(), "old");
  assert.deepEqual(dotfiles(h), []);
});

test("atomic: SIGTERM after the exchange swaps back and exits 143", async () => {
  const h = harness();
  const sig = new EventEmitter();
  let exitCode = null;
  const exited = new Promise((resolve) => { h.deps.exit = (c) => { exitCode = c; resolve(); }; });
  h.deps.swapPaths = h.fakeSwap;
  const inner = h.deps.execFile;
  // Release mode (dev never exchanges: it keeps no old copy): the signal lands during the re-verify of the exchanged-in bundle.
  h.deps.execFile = async (cmd, args) => {
    if (cmd.endsWith("codesign") && args[0] === "--verify" && args[3] === path.join(h.dest, "Pulse.app")) { sig.emit("SIGTERM"); await exited; }
    return inner(cmd, args);
  };
  h.deps.signalTarget = sig;
  const p = installApp(h.opts, h.deps).catch(() => {});
  await exited;
  assert.equal(exitCode, 143);
  assert.equal(h.marker(), "old");
  assert.deepEqual(dotfiles(h), []);
  await p;
});

for (const [name, swapAt, expectMarker] of [
  ["before the exchange", "before", "old"],
  ["after the exchange, before trash", "after", "new"],
]) {
  test(`atomic: SIGKILL ${name}: installed never absent; recovery leaves one valid bundle (${expectMarker}), idempotently`, async () => {
    const h = harness();
    h.deps.swapPaths = h.fakeSwap;
    const c = crashing(h, { swapAt });
    await assert.rejects(installApp(h.opts, c.deps), /kill (before|after) swap/);
    assert.ok(fs.existsSync(path.join(h.dest, "Pulse.app")), "destination present at the moment of the kill");
    assert.ok(fs.existsSync(journalPathFor(h.dest, "Pulse.app")));
    const rd = { ...recoverDeps(h), swapPaths: h.fakeSwap };
    const r1 = await recoverInstalls(rd);
    assert.equal(r1.length, 1);
    assert.deepEqual(bundles(h), ["Pulse.app"]);
    assert.equal(h.marker(), expectMarker);
    assert.deepEqual(dotfiles(h), []);
    assert.deepEqual(await recoverInstalls(rd), []);
    if (swapAt === "after") assert.equal(r1[0].action, "rolled-forward");
    else assert.equal(r1[0].action, "cleared");
  });
}

test("atomic: kill after the exchange with a bad new bundle rolls back by swapping", async () => {
  const h = harness();
  h.deps.swapPaths = h.fakeSwap;
  const c = crashing(h, { swapAt: "after" });
  await assert.rejects(installApp(h.opts, c.deps), /kill after swap/);
  h.deps.execFile = ((orig) => async (cmd, args) => { if (cmd.endsWith("codesign") && args[0] === "--verify") throw new Error("broken"); return orig(cmd, args); })(h.deps.execFile);
  const r = await recoverInstalls({ ...recoverDeps(h), swapPaths: h.fakeSwap });
  assert.equal(r[0].action, "rolled-back");
  assert.equal(h.marker(), "old");
  assert.deepEqual(bundles(h), ["Pulse.app"]);
  assert.deepEqual(dotfiles(h), []);
});

test("atomic: a verified journal rolls forward by keeping the old bundle as .previous, idempotently", async () => {
  const h = harness();
  h.deps.swapPaths = h.fakeSwap;
  const c = crashing(h, { swapAt: "after" });
  await assert.rejects(installApp(h.opts, c.deps), /kill after swap/);
  const journal = journalPathFor(h.dest, "Pulse.app");
  const j = JSON.parse(fs.readFileSync(journal, "utf8"));
  fs.writeFileSync(journal, JSON.stringify({ ...j, phase: "verified" }));
  fs.mkdirSync(path.join(h.dest, "Pulse.app.previous"));
  fs.writeFileSync(path.join(h.dest, "Pulse.app.previous", "older"), "an older previous");
  const r1 = await recoverInstalls({ ...recoverDeps(h), swapPaths: h.fakeSwap });
  assert.equal(r1[0].action, "rolled-forward");
  assert.equal(fs.readFileSync(path.join(h.dest, "Pulse.app.previous", "Contents", "marker"), "utf8"), "old");
  assert.ok(!fs.existsSync(path.join(h.dest, "Pulse.app.previous", "older")), "only one .previous is kept");
  assert.deepEqual(dotfiles(h), []);
  assert.deepEqual(await recoverInstalls({ ...recoverDeps(h), swapPaths: h.fakeSwap }), []);
});

test("mixed-mode recovery: a two-rename journal still recovers when a helper is available", async () => {
  const h = harness();
  const c = crashing(h, { at: (from, to) => (to.endsWith(".app.previous") ? "after" : null) });
  await assert.rejects(installApp(h.opts, c.deps), /kill after rename/);
  const j = JSON.parse(fs.readFileSync(journalPathFor(h.dest, "Pulse.app"), "utf8"));
  assert.equal(j.swap, "two-rename");
  await recoverInstalls({ ...recoverDeps(h), swapPaths: h.fakeSwap });
  assert.equal(h.marker(), "old");
});

test("resolveSwapHelper order: RIGHTKIT_SWAP, then shipped bin/darwin, then PATH", () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "swap-resolve-"));
  const mk = (p) => { fs.mkdirSync(path.dirname(p), { recursive: true }); fs.writeFileSync(p, "#!/bin/sh\n"); return p; };
  const envHelper = mk(path.join(root, "env", "rightkit-swap"));
  const shipped = mk(path.join(root, "pkg", "bin", "darwin", "rightkit-swap"));
  const onPath = mk(path.join(root, "bin", "rightkit-swap"));
  const base = { PATH: path.dirname(onPath) };
  const pkgDir = path.join(root, "pkg");
  assert.equal(resolveSwapHelper({ env: { ...base, RIGHTKIT_SWAP: envHelper }, pkgDir }), envHelper);
  assert.equal(resolveSwapHelper({ env: base, pkgDir }), shipped);
  assert.equal(resolveSwapHelper({ env: { ...base, RIGHTKIT_SWAP: path.join(root, "missing") }, pkgDir }), shipped);
  assert.equal(resolveSwapHelper({ env: base, pkgDir: path.join(root, "none") }), onPath);
  assert.equal(resolveSwapHelper({ env: { PATH: "" }, pkgDir: path.join(root, "none") }), null);
});

test("createSwapPaths maps helper exit codes: 0 ok, 3 ENOTSUP, missing helper ENOHELPER, other EIO", () => {
  const calls = [];
  const exec = (status) => (file, args) => { calls.push([file, ...args]); if (status === 0) return ""; const e = new Error("fail"); e.status = status; e.stderr = status === 3 ? "rightkit-swap: SWAP_UNSUPPORTED: x" : "rightkit-swap: SWAP_IO: y"; throw e; };
  const resolve = () => "/opt/rightkit-swap";
  createSwapPaths({ resolve, execFileSync: exec(0) })("/a", "/b");
  assert.deepEqual(calls[0], ["/opt/rightkit-swap", "/a", "/b"]);
  assert.throws(() => createSwapPaths({ resolve, execFileSync: exec(3) })("/a", "/b"), { code: "ENOTSUP" });
  assert.throws(() => createSwapPaths({ resolve, execFileSync: exec(1) })("/a", "/b"), { code: "EIO" });
  assert.throws(() => createSwapPaths({ resolve: () => null, execFileSync: exec(0) })("/a", "/b"), { code: "ENOHELPER" });
  const enoent = () => { const e = new Error("spawn"); e.code = "ENOENT"; throw e; };
  assert.throws(() => createSwapPaths({ resolve, execFileSync: enoent })("/a", "/b"), { code: "ENOHELPER" });
});

// ---- data safety and no dev backup (Orthic-Labs/rightkit#348) ----

const DATA_FILES = [["coderight.db", "db-bytes"], ["coderight.db-wal", "wal-bytes"], [path.join("models", "x.bin"), "model-bytes"]];
const seedData = (h) => { for (const [name, text] of DATA_FILES) { fs.mkdirSync(path.dirname(path.join(h.dest, name)), { recursive: true }); fs.writeFileSync(path.join(h.dest, name), text); } };
const readData = (h) => DATA_FILES.map(([name]) => [name, fs.readFileSync(path.join(h.dest, name), "utf8")]);
// The only thing the macOS install ever copies is the app bundle: every ditto names the source app or the staged bundle, never a data file.
const assertOnlyBundleCopied = (h) => {
  for (const call of h.calls.filter((c) => c[0].endsWith("ditto"))) {
    assert.ok(call.slice(1).every((arg) => !/coderight|models/.test(arg)), `ditto touched a data path: ${call.join(" ")}`);
  }
};

for (const dev of [false, true]) {
  const mode = dev ? "dev" : "release";
  test(`${mode}: data files beside the app are untouched and never copied, on a successful install`, { skip: WINDOWS_SKIP }, async () => {
    const h = harness();
    seedData(h);
    if (dev) h.opts = { ...h.opts, dmg: null, zip: path.join(h.root, "p.zip"), dev: true };
    const before = readData(h);
    await installApp(h.opts, h.deps);
    assert.deepEqual(readData(h), before);
    assertOnlyBundleCopied(h);
    assert.equal(h.marker(), "new");
    const names = fs.readdirSync(h.dest).sort();
    assert.deepEqual(names, dev ? ["Pulse.app", "coderight.db", "coderight.db-wal", "models"] : ["Pulse.app", "Pulse.app.previous", "coderight.db", "coderight.db-wal", "models"]);
  });

  test(`${mode}: data files beside the app are untouched and never copied, on a failed install`, { skip: WINDOWS_SKIP }, async () => {
    const h = harness({ fail: { reverify: "bad after swap" } });
    seedData(h);
    if (dev) h.opts = { ...h.opts, dmg: null, zip: path.join(h.root, "p.zip"), dev: true };
    const before = readData(h);
    await assert.rejects(installApp(h.opts, h.deps), /codesign-verify/);
    assert.deepEqual(readData(h), before);
    assertOnlyBundleCopied(h);
    if (dev) {
      assert.equal(fs.existsSync(path.join(h.dest, "Pulse.app")), false, "dev: the failed copy is removed and the old app is NOT restored");
      assert.deepEqual(fs.readdirSync(h.dest).sort(), ["coderight.db", "coderight.db-wal", "models"], "no .previous or backup of any kind");
    } else {
      assert.equal(h.marker(), "old", "release: the old bundle is renamed back");
    }
  });
}

test("dev: a stale .previous left by an older RightKit is removed, and a failed dev install says to reinstall from an older DMG", { skip: WINDOWS_SKIP }, async () => {
  const h = harness();
  fs.mkdirSync(path.join(h.dest, "Pulse.app.previous", "Contents"), { recursive: true });
  fs.writeFileSync(path.join(h.dest, "Pulse.app.previous", "Contents", "marker"), "ancient");
  h.opts = { ...h.opts, dmg: null, zip: path.join(h.root, "p.zip"), dev: true };
  await installApp(h.opts, h.deps);
  assert.deepEqual(fs.readdirSync(h.dest), ["Pulse.app"]);

  const f = harness({ fail: { reverify: "bad after swap" } });
  f.opts = { ...f.opts, dmg: null, zip: path.join(f.root, "p.zip"), dev: true };
  await assert.rejects(installApp(f.opts, f.deps), /reinstall from an older DMG/);
});

test("dev never swaps atomically: no exchange helper call, no old bundle parked at the staged path", { skip: WINDOWS_SKIP }, async () => {
  const h = harness();
  let exchanged = 0;
  h.deps.swapPaths = (a, b) => { exchanged += 1; h.fakeSwap(a, b); };
  h.opts = { ...h.opts, dmg: null, zip: path.join(h.root, "p.zip"), dev: true };
  const r = await installApp(h.opts, h.deps);
  assert.equal(exchanged, 0);
  assert.equal(r.swap, "replace");
  assert.equal(r.previous, null);
  assert.deepEqual(fs.readdirSync(h.dest), ["Pulse.app"]);
});

test("recovery of a dev journal from an older RightKit never keeps the old bundle as .previous", { skip: WINDOWS_SKIP }, async () => {
  const h = harness();
  const installed = path.join(h.dest, "Pulse.app");
  const backup = `${installed}.previous`;
  fs.mkdirSync(path.join(backup, "Contents"), { recursive: true });
  fs.writeFileSync(path.join(backup, "Contents", "marker"), "old-parked");
  fs.writeFileSync(journalPathFor(h.dest, "Pulse.app"), JSON.stringify({ schema: 1, appName: "Pulse.app", pid: 1, phase: "verified", dev: true, installed, backup, staged: path.join(h.dest, ".Pulse.rightkit-install-1.app"), mount: null, mountRoot: null }));
  const results = await recoverInstalls({ dest: h.dest, appName: "Pulse.app", run: h.deps.execFile, fsImpl: fs, home: h.home, now: () => new Date(), event: () => {}, sleep: async () => {}, isAlive: () => false, selfPid: 2, swapPaths: h.deps.swapPaths });
  assert.equal(results[0].action, "rolled-forward");
  assert.equal(fs.existsSync(backup), false);
});
