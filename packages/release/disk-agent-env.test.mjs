import assert from "node:assert/strict";
import test from "node:test";
import { AGENT_ENV_TABLE, agentEnvMap, checkAgentEnv, defaultExternalRoot, pointsAtInternalDrive, probeDirectory, probeLocations, renderAgentEnv, renderProbe, runDiskCommand, runProbe } from "./disk-command.mjs";

// In-memory fs: `dirs` is the set of existing directories, `files` the live files; `blocked` directories refuse writes.
function fakeFs({ dirs = [], blocked = [], failUnlink = false } = {}) {
  const dirSet = new Set(dirs);
  const files = new Set();
  const log = [];
  return {
    files, log, dirSet,
    existsSync: (p) => dirSet.has(p) || files.has(p),
    statSync: (p) => ({ isDirectory: () => dirSet.has(p) }),
    mkdirSync: (p) => { log.push(["mkdir", p]); dirSet.add(p); },
    writeFileSync: (p) => {
      const dir = [...dirSet].filter((d) => p.startsWith(d)).sort((a, b) => b.length - a.length)[0];
      if (blocked.includes(dir)) { const e = new Error("denied"); e.code = "EACCES"; throw e; }
      log.push(["write", p]); files.add(p);
    },
    unlinkSync: (p) => { if (failUnlink) { const e = new Error("busy"); e.code = "EBUSY"; throw e; } log.push(["unlink", p]); files.delete(p); },
    realpathSync: (p) => p,
  };
}

const NAMES = ["TEMP", "TMP", "TMPDIR", "CLAUDE_CODE_TMPDIR", "XDG_CACHE_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME", "XDG_CONFIG_HOME", "npm_config_cache", "PNPM_STORE_DIR", "YARN_CACHE_FOLDER", "PIP_CACHE_DIR", "UV_CACHE_DIR", "HF_HOME", "TORCH_HOME", "TRANSFORMERS_CACHE", "PLAYWRIGHT_BROWSERS_PATH", "PUPPETEER_CACHE_DIR", "NUGET_PACKAGES", "GOCACHE", "GOMODCACHE", "DOTNET_CLI_HOME", "ELECTRON_CACHE", "DENO_DIR"];

test("defaultExternalRoot per platform", () => {
  assert.equal(defaultExternalRoot("win32"), "D:\\");
  assert.equal(defaultExternalRoot("darwin"), "/Volumes/D");
  assert.throws(() => defaultExternalRoot("linux"), /unsupported/);
});

test("agentEnvMap follows the RustRight D: layout, never CARGO_TARGET_DIR", () => {
  const win = agentEnvMap({ root: "D:\\", platform: "win32" });
  assert.deepEqual(win.map((m) => m.name), NAMES);
  const at = (n) => win.find((m) => m.name === n).value;
  for (const n of ["TEMP", "TMP", "TMPDIR", "CLAUDE_CODE_TMPDIR"]) assert.equal(at(n), "D:\\agent-cache\\tmp");
  assert.equal(at("XDG_CACHE_HOME"), "D:\\agent-cache\\xdg");
  assert.equal(at("XDG_DATA_HOME"), "D:\\agent-cache\\xdg-data");
  assert.equal(at("XDG_STATE_HOME"), "D:\\agent-cache\\xdg-state");
  assert.equal(at("XDG_CONFIG_HOME"), "D:\\agent-cache\\xdg-config");
  assert.equal(at("PNPM_STORE_DIR"), "D:\\agent-cache\\pnpm-store");
  assert.equal(at("NUGET_PACKAGES"), "D:\\agent-cache\\nuget");
  assert.equal(at("npm_config_cache"), "D:\\.dev-cache\\npm-cache");
  assert.equal(at("PIP_CACHE_DIR"), "D:\\.dev-cache\\pip");
  assert.equal(at("UV_CACHE_DIR"), "D:\\.dev-cache\\uv");
  assert.equal(at("HF_HOME"), "D:\\.dev-cache\\huggingface");
  assert.equal(at("GOCACHE"), "D:\\agent-cache\\go-build");
  assert.equal(at("DENO_DIR"), "D:\\agent-cache\\deno");
  assert.ok(!win.some((m) => m.name === "CARGO_TARGET_DIR"));
  assert.ok(!win.some((m) => m.name === "CODEX_HOME" || m.name === "CLAUDE_CONFIG_DIR"));
  const mac = agentEnvMap({ root: "/Volumes/D", platform: "darwin", includeAgentHomes: true });
  assert.equal(mac.find((m) => m.name === "TEMP").value, "/Volumes/D/agent-cache/tmp");
  assert.equal(mac.find((m) => m.name === "HF_HOME").value, "/Volumes/D/.dev-cache/huggingface");
  assert.equal(mac.find((m) => m.name === "CODEX_HOME").value, "/Volumes/D/agent-home/codex");
  assert.equal(win.length + 2, mac.length);
  assert.equal(agentEnvMap({ root: "D:\\", platform: "win32", includeAgentHomes: true }).find((m) => m.name === "CLAUDE_CONFIG_DIR").value, "D:\\agent-home\\claude");
  assert.ok(AGENT_ENV_TABLE.every((e) => Array.isArray(e.segments)));
});

test("renderAgentEnv formats and quotes", () => {
  const map = [{ name: "TEMP", value: "D:\\it's" }];
  assert.match(renderAgentEnv(map, { format: "ps1", root: "D:\\" }), /\$env:TEMP = 'D:\\it''s'/);
  assert.match(renderAgentEnv(map, { format: "sh", root: "D:\\" }), /export TEMP='D:\\it'\\''s'/);
  assert.match(renderAgentEnv(map, { format: "cmd", root: "D:\\" }), /set "TEMP=D:\\it's"/);
  assert.match(renderAgentEnv(map, { format: "ps1", root: "D:\\" }), /# CARGO_TARGET_DIR is intentionally NOT set/);
  assert.equal(JSON.parse(renderAgentEnv(map, { format: "json", root: "D:\\" })).env.TEMP, "D:\\it's");
  assert.throws(() => renderAgentEnv(map, { format: "xml" }), /unknown --format/);
});

test("pointsAtInternalDrive", () => {
  assert.equal(pointsAtInternalDrive("C:\\Users\\a\\AppData\\Local\\Temp", { platform: "win32" }), true);
  assert.equal(pointsAtInternalDrive("c:\\x", { platform: "win32" }), true);
  assert.equal(pointsAtInternalDrive("D:\\x", { platform: "win32" }), false);
  assert.equal(pointsAtInternalDrive("E:\\x", { platform: "win32", env: { SystemDrive: "E:" } }), true);
  assert.equal(pointsAtInternalDrive("relative\\x", { platform: "win32" }), true);
  assert.equal(pointsAtInternalDrive("/Users/a/Library/Caches", { platform: "darwin" }), true);
  assert.equal(pointsAtInternalDrive("/var/folders/x", { platform: "darwin" }), true);
  assert.equal(pointsAtInternalDrive("/Volumes/D/x", { platform: "darwin" }), false);
});

test("agent-env prints the map and creates directories only under the root", async () => {
  const fs = fakeFs({ dirs: ["D:\\"] });
  const lines = [];
  const r = await runDiskCommand(["agent-env", "--format", "json"], { platform: "win32", env: {}, stdout: (l) => lines.push(l), deps: { fs, nonce: "n1" } });
  assert.equal(r.exitCode, 0);
  const out = JSON.parse(lines.join("\n"));
  assert.equal(out.env.TMPDIR, "D:\\agent-cache\\tmp");
  assert.equal(fs.files.size, 0, "writability probe file removed");
  for (const [op, p] of fs.log) if (op === "mkdir") assert.ok(p.startsWith("D:\\agent-cache\\") || p.startsWith("D:\\.dev-cache\\"), p);
});

test("agent-env fails closed: missing root, unwritable root, internal root print nothing and exit non-zero", async () => {
  for (const [fs, root] of [[fakeFs({ dirs: [] }), "D:\\"], [fakeFs({ dirs: ["D:\\"], blocked: ["D:\\"] }), "D:\\"], [fakeFs({ dirs: ["C:\\"] }), "C:\\"]]) {
    const lines = [];
    const errors = [];
    const r = await runDiskCommand(["agent-env", "--root", root], { platform: "win32", env: {}, stdout: (l) => lines.push(l), deps: { fs, stderr: (l) => errors.push(l) } });
    assert.equal(r.exitCode, 1);
    assert.deepEqual(lines, []);
    assert.match(errors.join(""), /refusing/);
    assert.deepEqual(fs.log.filter(([op]) => op === "mkdir"), []);
  }
  // macOS: a root that resolves to the boot volume is refused
  const mac = fakeFs({ dirs: ["/Volumes/D"] });
  mac.realpathSync = () => "/";
  const lines = [];
  const r = await runDiskCommand(["agent-env"], { platform: "darwin", env: {}, stdout: (l) => lines.push(l), deps: { fs: mac, stderr: () => {} } });
  assert.equal(r.exitCode, 1);
  assert.deepEqual(lines, []);
});

test("agent-env --include-agent-homes adds the two homes", async () => {
  const fs = fakeFs({ dirs: ["/Volumes/D"] });
  const lines = [];
  await runDiskCommand(["agent-env", "--format", "sh", "--include-agent-homes"], { platform: "darwin", env: {}, stdout: (l) => lines.push(l), deps: { fs } });
  assert.match(lines.join("\n"), /export CODEX_HOME='\/Volumes\/D\/agent-home\/codex'/);
  assert.match(lines.join("\n"), /export CLAUDE_CONFIG_DIR=/);
});

test("agent-env rejects unknown options and formats", async () => {
  await assert.rejects(() => runDiskCommand(["agent-env", "--bogus"], { platform: "win32", deps: { fs: fakeFs() } }), /unknown option/);
  await assert.rejects(() => runDiskCommand(["agent-env", "--format", "xml"], { platform: "win32", deps: { fs: fakeFs() } }), /unknown --format/);
});

test("checkAgentEnv classifies ok, elsewhere, internal and unset", () => {
  const map = agentEnvMap({ root: "D:\\", platform: "win32" }).slice(0, 4);
  const env = { temp: "D:\\agent-cache\\tmp", TMP: "C:\\Users\\a\\AppData\\Local\\Temp", TMPDIR: "E:\\other" };
  const rows = checkAgentEnv(map, { env, platform: "win32", root: "D:\\" });
  assert.deepEqual(rows.map((r) => r.status), ["ok", "internal", "elsewhere", "unset"]);
});

test("agent-env --check lists variables on the internal drive and exits 1", async () => {
  const fs = fakeFs({ dirs: ["D:\\"] });
  const lines = [];
  const r = await runDiskCommand(["agent-env", "--check"], { platform: "win32", env: { TEMP: "C:\\Users\\a\\AppData\\Local\\Temp" }, stdout: (l) => lines.push(l), deps: { fs } });
  assert.equal(r.exitCode, 1);
  assert.match(lines.join("\n"), /INTERNAL\s+TEMP\s+C:\\Users/);
  assert.equal(fs.log.filter(([op]) => op === "mkdir").length, 0, "--check creates nothing");
  // all set to the map: exits 0
  const env = Object.fromEntries(agentEnvMap({ root: "D:\\", platform: "win32" }).map((m) => [m.name, m.value]));
  const ok = await runDiskCommand(["agent-env", "--check"], { platform: "win32", env, stdout: () => {}, deps: { fs: fakeFs({ dirs: ["D:\\"] }) } });
  assert.equal(ok.exitCode, 0);
});

test("probeDirectory creates and removes one uniquely named 0-byte file", () => {
  const fs = fakeFs({ dirs: ["/tmp"] });
  const r = probeDirectory("/tmp", { fs, nonce: "abc", platform: "darwin" });
  assert.equal(r.outcome, "allowed");
  assert.equal(r.code, null);
  assert.equal(fs.files.size, 0);
  assert.deepEqual(fs.log.map(([op]) => op), ["write", "unlink"]);
  assert.match(fs.log[0][1], /\.rk-disk-probe-\d+-abc$/);
});

test("probeDirectory reports blocked, missing and a failed removal", () => {
  const blocked = probeDirectory("/ro", { fs: fakeFs({ dirs: ["/ro"], blocked: ["/ro"] }), platform: "darwin" });
  assert.deepEqual([blocked.outcome, blocked.code], ["blocked", "EACCES"]);
  const missing = probeDirectory("/nope", { fs: fakeFs(), platform: "darwin" });
  assert.deepEqual([missing.outcome, missing.code], ["error", "ENOENT"]);
  const stuck = probeDirectory("/tmp", { fs: fakeFs({ dirs: ["/tmp"], failUnlink: true }), platform: "darwin" });
  assert.equal(stuck.outcome, "allowed");
  assert.equal(stuck.code, "UNLINK_EBUSY");
  assert.equal(stuck.leftover, true);
});

test("probeLocations and runProbe cover profile, local temp, OS temp and external root", () => {
  const locs = probeLocations({ platform: "win32", env: { LOCALAPPDATA: "C:\\Users\\a\\AppData\\Local" }, home: "C:\\Users\\a", tmpdir: "C:\\Windows\\Temp" });
  assert.deepEqual(locs.map((l) => l.location), ["profile", "local-temp", "os-temp", "external-root"]);
  assert.deepEqual(locs.map((l) => l.dir), ["C:\\Users\\a", "C:\\Users\\a\\AppData\\Local\\Temp", "C:\\Windows\\Temp", "D:\\"]);
  const fs = fakeFs({ dirs: ["C:\\Users\\a", "C:\\Users\\a\\AppData\\Local\\Temp", "C:\\Windows\\Temp", "D:\\"], blocked: ["C:\\Users\\a", "C:\\Windows\\Temp"] });
  const { report } = runProbe({ platform: "win32", env: { LOCALAPPDATA: "C:\\Users\\a\\AppData\\Local" }, home: "C:\\Users\\a", tmpdir: "C:\\Windows\\Temp", fs, host: "dell" });
  assert.deepEqual(report.results.map((r) => r.outcome), ["blocked", "allowed", "blocked", "allowed"]);
  assert.equal(report.internalWritable, true, "local-temp accepted the probe file");
  assert.equal(fs.files.size, 0);
  assert.match(renderProbe(report), /BLOCKED\s+profile/);
  const sealed = runProbe({ platform: "win32", env: { LOCALAPPDATA: "C:\\Users\\a\\AppData\\Local" }, home: "C:\\Users\\a", tmpdir: "C:\\Windows\\Temp", host: "dell", fs: fakeFs({ dirs: ["C:\\Users\\a", "C:\\Users\\a\\AppData\\Local\\Temp", "C:\\Windows\\Temp", "D:\\"], blocked: ["C:\\Users\\a", "C:\\Users\\a\\AppData\\Local\\Temp", "C:\\Windows\\Temp"] }) });
  assert.equal(sealed.report.internalWritable, false);
  const mac = probeLocations({ platform: "darwin", env: { TMPDIR: "/var/folders/x/T/" }, home: "/Users/a", tmpdir: "/tmp" });
  assert.equal(mac[1].dir, "/var/folders/x/T/");
  assert.equal(mac[3].dir, "/Volumes/D");
});

test("probe --json emits exactly the v1 schema; exits 0 when sealed and 1 when an internal location is writable", async () => {
  const sealedFs = fakeFs({ dirs: ["/Users/a", "/tmp", "/Volumes/D"], blocked: ["/Users/a", "/tmp"] });
  const lines = [];
  const r = await runDiskCommand(["probe", "--json"], { platform: "darwin", env: { TMPDIR: "/tmp" }, stdout: (l) => lines.push(l), deps: { fs: sealedFs, probe: { home: "/Users/a", tmpdir: "/tmp", host: "mac" } } });
  assert.equal(r.exitCode, 0);
  const out = JSON.parse(lines.join(""));
  assert.deepEqual(Object.keys(out), ["schema", "host", "root", "results", "internalWritable"]);
  assert.equal(out.schema, "rightkit.disk.probe.v1");
  assert.equal(out.host, "mac");
  assert.equal(out.root, "/Volumes/D");
  assert.deepEqual(out.results.map((x) => Object.keys(x)), Array(4).fill(["location", "path", "outcome", "code"]));
  assert.deepEqual(out.results.map((x) => x.outcome), ["blocked", "blocked", "blocked", "allowed"]);
  assert.equal(out.internalWritable, false);
  assert.equal(sealedFs.files.size, 0);
  const openFs = fakeFs({ dirs: ["/Users/a", "/tmp", "/Volumes/D"], blocked: ["/Users/a"] });
  const bad = await runDiskCommand(["probe", "--json"], { platform: "darwin", env: { TMPDIR: "/tmp" }, stdout: () => {}, deps: { fs: openFs, probe: { home: "/Users/a", tmpdir: "/tmp", host: "mac" } } });
  assert.equal(bad.exitCode, 1);
  assert.equal(bad.report.internalWritable, true);
  assert.equal(openFs.files.size, 0);
});
