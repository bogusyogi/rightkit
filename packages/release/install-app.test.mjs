import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { installApp, matchPids, mountPointsFromAttach, parseArgs, parseTeamId, parsePlist, trashName, main } from "./install-app.mjs";

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
  const calls = [];
  const kills = [];
  let alive = running;
  const execFile = async (cmd, args) => {
    calls.push([cmd, ...args]);
    const key = path.basename(cmd) + ":" + (args[0] === "stapler" ? "stapler" : args[0]);
    const fx = (name) => { if (fail[name]) { const e = new Error(fail[name]); e.stderr = fail[name]; throw e; } };
    if (cmd.endsWith("hdiutil") && args[0] === "attach") return { stdout: `<plist><dict><key>system-entities</key><array><dict><key>mount-point</key><string>${mount}</string></dict></array></dict></plist>`, stderr: "" };
    if (cmd.endsWith("hdiutil")) return { stdout: "", stderr: "" };
    if (cmd.endsWith("codesign") && args[0] === "--verify") { fx(args[3].startsWith(mount) ? "verify" : "reverify"); return { stdout: "", stderr: "" }; }
    if (cmd.endsWith("codesign") && args[0] === "-dvv") return { stdout: "", stderr: `TeamIdentifier=${args[1].startsWith(mount) ? team : installedTeam}\n` };
    if (cmd.endsWith("codesign")) return { stdout: "", stderr: "" };
    if (cmd.endsWith("spctl")) { fx("spctl"); return { stdout: "", stderr: "" }; }
    if (cmd.endsWith("xcrun")) { fx("stapler"); return { stdout: "", stderr: "" }; }
    if (cmd.endsWith("plutil")) return { stdout: JSON.stringify({ CFBundleIdentifier: bundleId }), stderr: "" };
    if (cmd.endsWith("ps")) return { stdout: alive ? `  555 ${dest}/Pulse.app/Contents/MacOS/Pulse\n` : "", stderr: "" };
    if (cmd.endsWith("ditto")) {
      fx("ditto");
      fs.cpSync(args[0], args[1], { recursive: true });
      return { stdout: "", stderr: "" };
    }
    if (cmd.endsWith("open")) return { stdout: "", stderr: "" };
    throw new Error(`unexpected ${cmd}`);
  };
  const events = [];
  const deps = { execFile, platform: "darwin", home, tmpdir: root, emit: (e) => events.push(e), sleep: async () => {}, kill: (pid, sig) => { kills.push([pid, sig]); if (sig === "SIGTERM" && !fail.stubborn) alive = false; if (sig === "SIGKILL") alive = false; } };
  const opts = { dmg: path.join(root, "p.dmg"), appName: null, bundleId: null, dest, relaunch: null, json: true };
  return { root, dest, home, deps, opts, calls, kills, events, marker: () => fs.readFileSync(path.join(dest, "Pulse.app", "Contents", "marker"), "utf8") };
}

test("happy path replaces, trashes old, quits with SIGTERM, relaunches hidden because it was running", async () => {
  const h = harness({ running: true });
  const r = await installApp(h.opts, h.deps);
  assert.equal(r.ok, true);
  assert.equal(h.marker(), "new");
  assert.deepEqual(h.kills, [[555, "SIGTERM"]]);
  assert.equal(fs.readFileSync(path.join(r.previous, "Contents", "marker"), "utf8"), "old");
  assert.deepEqual(h.events.map((e) => e.event), ["start", "verified", "quit", "replaced", "relaunched", "done"]);
  const open = h.calls.find((c) => c[0] === "/usr/bin/open");
  assert.deepEqual(open, ["/usr/bin/open", "-g", "-j", "-b", "dev.orthic.pulse"]);
  assert.ok(h.calls.some((c) => c[0].endsWith("hdiutil") && c[1] === "detach"));
  assert.ok(!h.calls.some((c) => /osascript/.test(c[0])));
});

test("not running and --no-relaunch: no open call; SIGKILL after stubborn process", async () => {
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

test("ditto failure restores the old bundle from Trash", async () => {
  const h = harness({ fail: { ditto: "disk full" } });
  await assert.rejects(installApp(h.opts, h.deps), /previous app restored/);
  assert.equal(h.marker(), "old");
});

test("fresh install into empty dest works without trash", async () => {
  const h = harness();
  fs.rmSync(path.join(h.dest, "Pulse.app"), { recursive: true });
  const r = await installApp(h.opts, h.deps);
  assert.equal(r.previous, null);
  assert.equal(h.marker(), "new");
});
