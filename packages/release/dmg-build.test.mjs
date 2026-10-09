import assert from "node:assert/strict";
import { copyFileSync, cpSync, existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, readlinkSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "./background-process.mjs";
import test from "node:test";
import { buildDmg, parseDmgArguments } from "./dmg-build.mjs";
import { dmgDSStore, backgroundAlias } from "./dmg-layout.mjs";
import { fileURLToPath } from "node:url";

function fixture(t) {
  const root = realpathSync(mkdtempSync(path.join(os.tmpdir(), "rk-dmg-test-"))), app = path.join(root, "Tést App.app"), out = path.join(root, "out.dmg");
  mkdirSync(path.join(app, "Contents"), { recursive: true });
  writeFileSync(path.join(app, "Contents", "Info.plist"), '<?xml version="1.0"?><plist version="1.0"><dict><key>CFBundleName</key><string>Fixture</string></dict></plist>');
  const background = path.join(root, "background.png");
  writeFileSync(background, Buffer.from("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAusB9Wl6uV8AAAAASUVORK5CYII=", "base64"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  return { root, app, out, background, volname: "Tést App" };
}

// Independent format reader: allocator TOC -> DSDB superblock -> B-tree leaf.
function readStore(bytes) {
  assert.equal(bytes.readUInt32BE(0), 1); assert.equal(bytes.toString("ascii", 4, 8), "Bud1");
  const allocator = bytes.readUInt32BE(8) + 4;
  assert.equal(bytes.readUInt32BE(16) + 4, allocator);
  const count = bytes.readUInt32BE(allocator), offsets = [];
  for (let i = 0; i < count; i++) offsets.push(bytes.readUInt32BE(allocator + 8 + i * 4));
  let p = allocator + 8 + Math.ceil(count / 256) * 1024;
  assert.equal(bytes.readUInt32BE(p), 1); p += 4;
  const length = bytes[p++]; assert.equal(bytes.toString("ascii", p, p + length), "DSDB"); p += length;
  const superBlock = (offsets[bytes.readUInt32BE(p)] & ~31) + 4;
  const nodeId = bytes.readUInt32BE(superBlock), node = (offsets[nodeId] & ~31) + 4;
  assert.equal(bytes.readUInt32BE(superBlock + 4), 0);
  assert.equal(bytes.readUInt32BE(superBlock + 12), 1);
  assert.equal(bytes.readUInt32BE(node), 0);
  const records = bytes.readUInt32BE(node + 4); assert.equal(bytes.readUInt32BE(superBlock + 8), records);
  p = node + 8; const result = {};
  const utf = (n) => { const s = Buffer.from(bytes.subarray(p, p + n * 2)).swap16().toString("utf16le"); p += n * 2; return s; };
  for (let i = 0; i < records; i++) {
    const chars = bytes.readUInt32BE(p); p += 4; const name = utf(chars);
    const code = bytes.toString("ascii", p, p + 4), type = bytes.toString("ascii", p + 4, p + 8); p += 8;
    let value;
    if (type === "blob") { const size = bytes.readUInt32BE(p); p += 4; value = bytes.subarray(p, p + size); p += size; }
    else if (type === "long") { value = bytes.readUInt32BE(p); p += 4; }
    else if (type === "bool") value = !!bytes[p++];
    else if (type === "type") { value = bytes.toString("ascii", p, p + 4); p += 4; }
    else assert.fail(`unexpected type ${type}`);
    result[`${name}/${code}`] = value;
  }
  assert.ok(p <= node + (1 << (offsets[nodeId] & 31)));
  return result;
}

function readPlist(bytes) {
  // Python's stdlib plistlib independently validates binary Finder plists.
  const code = "import sys,plistlib,json; p=plistlib.loads(sys.stdin.buffer.read()); print(json.dumps(p,default=lambda b:b.hex()))";
  const result = spawnSync(process.platform === "win32" ? "py" : "python3", [...(process.platform === "win32" ? ["-3.11"] : []), "-c", code], { input: bytes, encoding: "utf8" });
  assert.equal(result.status, 0, result.stderr); return JSON.parse(result.stdout);
}

test("DS_Store parses back window, icon coordinates, Unicode names & background alias", (t) => {
  const f = fixture(t), mount = path.join(f.root, "mount");
  mkdirSync(path.join(mount, ".background"), { recursive: true }); copyFileSync(f.background, path.join(mount, ".background", "background.png"));
  // Fixture FS may have 64-bit inodes; the production image is HFS+ (32-bit CNIDs).
  const stat = () => ({ ino: 42, birthtimeMs: 1700000000000 });
  const alias = backgroundAlias({ mount, volname: f.volname, relative: ".background/background.png", stat });
  const entries = readStore(dmgDSStore({ appName: path.basename(f.app), iconSize: 96,
    appPosition: [101, 202], applicationsPosition: [303, 202], window: [600, 380], alias }));
  assert.equal(entries["./vSrn"], 1); assert.equal(entries["./icvl"], "icnv");
  assert.equal(readPlist(entries["./bwsp"]).WindowBounds, "{{100, 100}, {600, 380}}");
  const icon = readPlist(entries["./icvp"]);
  assert.equal(icon.iconSize, 96); assert.equal(icon.backgroundType, 2); assert.equal(icon.arrangeBy, "none");
  const parsedAlias = Buffer.from(icon.backgroundImageAlias, "hex");
  assert.equal(parsedAlias.readUInt16BE(4), alias.length); assert.equal(parsedAlias.readUInt16BE(6), 2);
  assert.equal(parsedAlias.readUInt32BE(114), 42);
  const tags = {}; let p = 150;
  while (parsedAlias.readInt16BE(p) !== -1) {
    const tag = parsedAlias.readUInt16BE(p), size = parsedAlias.readUInt16BE(p + 2); p += 4;
    tags[tag] = parsedAlias.subarray(p, p + size); p += size + size % 2;
  }
  assert.equal(tags[18].toString(), "/.background/background.png"); assert.equal(tags[19].toString(), mount);
  assert.equal(Buffer.from(tags[15].subarray(2)).swap16().toString("utf16le"), f.volname);
  assert.equal(entries[`${path.basename(f.app)}/Iloc`].readUInt32BE(0), 101);
  assert.equal(entries["Applications/Iloc"].readUInt32BE(4), 202);
  assert.equal(readPlist(readStore(dmgDSStore({ appName: "Fixture.app" }))["./icvp"]).backgroundType, 0);
});

function mockCommands(f, calls, failPhase) {
  return (tool, args, options) => {
    calls.push([tool, ...args]); assert.equal(options.shell, false); assert.ok(options.timeout);
    if (args[0] === failPhase) return { status: 1, stderr: "injected" };
    if (tool.endsWith("ditto")) cpSync(args[0], args[1], { recursive: true });
    if (args[0] === "attach") {
      const mount = args[args.indexOf("-mountpoint") + 1], work = path.dirname(mount);
      cpSync(path.join(work, "stage"), mount, { recursive: true, verbatimSymlinks: true });
      assert.equal(readlinkSync(path.join(mount, "Applications")), "/Applications");
    }
    if (args[0] === "detach" && args[1] !== "-force" && existsSync(path.join(args[1], ".DS_Store"))) {
      const ds = readStore(readFileSync(path.join(args[1], ".DS_Store")));
      assert.equal(ds["./icvl"], "icnv");
    }
    if (args[0] === "convert") writeFileSync(args[args.indexOf("-o") + 1], "UDZO fixture");
    return { status: 0, stdout: "", stderr: "" };
  };
}

test("headless builder uses hdiutil, stages Applications, verifies UDZO & cleans work", (t) => {
  const f = fixture(t), calls = [], events = [];
  // Mock uses no background so alias never queries non-HFS+ fixture inodes.
  const result = buildDmg({ ...f, background: undefined }, { platform: "darwin", run: mockCommands(f, calls), log: (e) => events.push(e) });
  assert.equal(result.format, "UDZO"); assert.equal(readFileSync(f.out, "utf8"), "UDZO fixture");
  assert.deepEqual(calls.slice(1).map((c) => c[1]), ["create", "attach", "detach", "convert", "verify"]);
  assert.ok(calls[2].includes("-nobrowse")); assert.ok(calls[2].includes("-noautoopen"));
  assert.ok(calls[4].includes("UDZO")); assert.doesNotMatch(JSON.stringify(calls), /osascript|Finder|bundle_dmg/);
  assert.equal(events.at(-1).event, "dmg-build-completed"); assert.ok(!readdirSync(f.root).some((n) => n.startsWith(".rightkit-dmg-")));
});

test("mocked branded DMG stages background & writes alias to mounted layout", (t) => {
  const f = fixture(t), calls = [];
  buildDmg(f, { platform: "darwin", run: mockCommands(f, calls), log() {},
    makeAlias(options) {
      assert.equal(options.volname, f.volname); assert.equal(options.relative, path.join(".background", "background.png"));
      assert.deepEqual(readFileSync(path.join(options.mount, options.relative)), readFileSync(f.background));
      return backgroundAlias({ ...options, stat: () => ({ ino: 42, birthtimeMs: 1700000000000 }) });
    } });
  assert.ok(existsSync(f.out));
});

for (const phase of ["attach", "detach", "convert", "verify"]) test(`DMG ${phase} failure preserves old output & detaches before cleanup`, (t) => {
  const f = fixture(t), calls = [], events = []; writeFileSync(f.out, "previous");
  assert.throws(() => buildDmg({ ...f, background: undefined }, { platform: "darwin", run: mockCommands(f, calls, phase), log: (e) => events.push(e), sleep() {} }), { code: "DMG_COMMAND_FAILED" });
  assert.equal(readFileSync(f.out, "utf8"), "previous"); assert.equal(events.some((e) => e.event === "dmg-build-failed"), true);
  if (phase === "detach") assert.ok(calls.some((c) => c[1] === "detach" && c.includes("-force")));
  if (phase === "attach") assert.ok(calls.some((c) => c[1] === "detach"));
  if (phase !== "detach") assert.ok(!readdirSync(f.root).some((n) => n.startsWith(".rightkit-dmg-")));
});

test("DMG CLI parses options & rejects invalid/native-host requests", (t) => {
  const f = fixture(t);
  assert.deepEqual(parseDmgArguments(["build", "--app", f.app, "--out", f.out, "--volname", f.volname, "--window", "660,400"]), { app: f.app, out: f.out, volname: f.volname, window: "660,400" });
  assert.throws(() => parseDmgArguments(["build", "--app"]), { code: "DMG_INVALID_OPTIONS" });
  assert.throws(() => parseDmgArguments(["build", "--wat", "x"]), { code: "DMG_INVALID_OPTIONS" });
  assert.throws(() => buildDmg(f, { platform: "win32" }), { code: "DMG_NATIVE_HOST_REQUIRED" });
  for (const opts of [{ iconSize: "bad" }, { window: "1,-2" }, { appPosition: "1" }, { volname: "bad/name" }]) {
    assert.throws(() => buildDmg({ ...f, ...opts }, { platform: "darwin", run: () => assert.fail(), log() {} }), { code: "DMG_INVALID_OPTIONS" });
  }
});

test("installed CLI dispatches dmg build & reports typed missing options", (t) => {
  const f = fixture(t), cli = fileURLToPath(new URL("./cli/right-release.mjs", import.meta.url));
  const help = spawnSync(process.execPath, [cli, "--help"], { encoding: "utf8", cwd: f.root });
  assert.equal(help.status, 0); assert.match(help.stdout, /dmg build --app/);
  const result = spawnSync(process.execPath, [cli, "dmg", "build"], { encoding: "utf8", cwd: f.root });
  assert.equal(result.status, 1);
  assert.match(result.stderr, process.platform === "darwin" ? /DMG_INVALID_OPTIONS/ : /DMG_NATIVE_HOST_REQUIRED/);
});

test("real native headless DMG roundtrip", { skip: process.platform !== "darwin" || process.env.RIGHTKIT_TEST_REAL_DMG !== "1" }, (t) => {
  const f = fixture(t); buildDmg(f);
  const mount = path.join(f.root, "check"); mkdirSync(mount);
  const attached = spawnSync("/usr/bin/hdiutil", ["attach", "-readonly", "-nobrowse", "-noautoopen", "-mountpoint", mount, f.out], { encoding: "utf8" });
  assert.equal(attached.status, 0, attached.stderr);
  try {
    assert.ok(existsSync(path.join(mount, path.basename(f.app), "Contents", "Info.plist")));
    assert.equal(readlinkSync(path.join(mount, "Applications")), "/Applications");
    const records = readStore(readFileSync(path.join(mount, ".DS_Store")));
    assert.equal(readPlist(records["./icvp"]).backgroundType, 2);
  } finally { assert.equal(spawnSync("/usr/bin/hdiutil", ["detach", mount]).status, 0); }
});

test("DMG detach retries 'Resource busy' with backoff, logs each retry, then succeeds without -force", (t) => {
  const f = fixture(t), calls = [], events = [], sleeps = []; let busy = 3;
  const base = mockCommands(f, calls);
  const run = (tool, args, options) => (args[0] === "detach" && !args.includes("-force") && busy-- > 0
    ? (calls.push([tool, ...args]), { status: 16, stderr: "Resource busy" }) : base(tool, args, options));
  const result = buildDmg({ ...f, background: undefined }, { platform: "darwin", run, log: (e) => events.push(e), sleep: (ms) => sleeps.push(ms) });
  assert.equal(result.format, "UDZO");
  assert.equal(calls.filter((c) => c[1] === "detach").length, 4); assert.ok(!calls.some((c) => c.includes("-force")));
  assert.deepEqual(sleeps, [500, 1000, 2000]);
  assert.equal(events.filter((e) => e.event === "hdiutil-detach-retry").length, 3);
});

test("DMG detach exhausts 5 tries then uses -force as last resort", (t) => {
  const f = fixture(t), calls = [], events = [], sleeps = [];
  const base = mockCommands(f, calls);
  const run = (tool, args, options) => (args[0] === "detach" && !args.includes("-force")
    ? (calls.push([tool, ...args]), { status: 16, stderr: "Resource busy" }) : base(tool, args, options));
  buildDmg({ ...f, background: undefined }, { platform: "darwin", run, log: (e) => events.push(e), sleep: (ms) => sleeps.push(ms) });
  const detaches = calls.filter((c) => c[1] === "detach");
  assert.equal(detaches.length, 6); assert.ok(detaches.at(-1).includes("-force"));
  assert.deepEqual(sleeps, [500, 1000, 2000, 4000]);
  assert.ok(events.some((e) => e.event === "hdiutil-detach-force"));
});
