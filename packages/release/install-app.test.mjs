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
  fs.writeFileSync(path.join(root, "p.zip"), "");
  const calls = [];
  const kills = [];
  let alive = running;
  const isNew = (p) => !p.startsWith(dest);
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
  const deps = { execFile, platform: "darwin", home, tmpdir: root, emit: (e) => events.push(e), sleep: async () => {}, kill: (pid, sig) => { kills.push([pid, sig]); if (sig === "SIGTERM" && !fail.stubborn) alive = false; if (sig === "SIGKILL") alive = false; } };
  const opts = { dmg: path.join(root, "p.dmg"), appName: null, bundleId: null, dest, relaunch: null, json: true };
  return { root, mount, dest, home, deps, opts, calls, kills, events, marker: () => fs.readFileSync(path.join(dest, "Pulse.app", "Contents", "marker"), "utf8") };
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

test("--dev zip install: skips spctl/stapler, clears quarantine on the new bundle only, flags dev", async () => {
  const h = harness({ running: true });
  h.opts = { ...h.opts, dmg: null, zip: path.join(h.root, "p.zip"), dev: true };
  const r = await installApp(h.opts, h.deps);
  assert.equal(r.ok, true);
  assert.equal(r.dev, true);
  assert.equal(h.marker(), "new");
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
