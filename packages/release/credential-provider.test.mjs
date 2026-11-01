// The managed OS credential provider (credential-provider.mjs): enrolment, resolution, its use by updater signing, and its tool route (tool-build.mjs).
// No real cargo runs and no real rightkit-credentials: the binary is a file of known bytes and its identity probe is a double.
import assert from "node:assert/strict";
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { CredentialProviderError, enrollCredentialProvider, main, resolveCredentialProvider, updaterCredentialProvider } from "./credential-provider.mjs";
import { execWithUpdaterCredential, protectedSecretStoreCommand } from "./protected-credentials.mjs";
import { tempRoot } from "./test-support/temp-roots.mjs";
import { runUnitTool } from "./tool-build.mjs";

const IS_CLI = { status: 1, stdout: '{"ok":false,"error":"invalid --namespace: required"}\n', stderr: "" };
const INSTALL = "rightkit-gate --to tool --unit cargo:rightkit-credentials";
const sha = (text) => crypto.createHash("sha256").update(text).digest("hex");

function fixture({ platform = "mac", hostPlatform = platform === "win" ? "win32" : "darwin", answer = IS_CLI } = {}) {
  const dataRoot = tempRoot("cred-provider-");
  const spawned = [];
  const spawn = (file, args) => { spawned.push([file, ...args]); return answer; };
  const binary = path.join(dataRoot, "built-rightkit-credentials"); fs.writeFileSync(binary, "bytes-1", { mode: 0o755 });
  const enroll = (extra = {}) => enrollCredentialProvider({ dataRoot, platform, binary, version: "0.4.3", spawn, hostPlatform, ...extra });
  return { dataRoot, spawned, spawn, binary, enroll, platform, hostPlatform };
}
const code = (fn) => { try { fn(); } catch (error) { assert.ok(error instanceof CredentialProviderError, String(error)); return error.code; } return null; };

test("enrolment copies the binary under <data>/credential-provider/<platform>/<version>/ and records its sha256; resolve returns it", () => {
  for (const platform of ["mac", "win"]) {
    const f = fixture({ platform });
    const r = f.enroll();
    const file = path.join(f.dataRoot, "credential-provider", platform, "0.4.3", platform === "win" ? "rightkit-credentials.exe" : "rightkit-credentials");
    assert.equal(r.providerFile, file);
    assert.equal(fs.readFileSync(file, "utf8"), "bytes-1");
    assert.equal(r.entry.sha256, sha("bytes-1"));
    assert.equal(r.rotated, false);
    assert.deepEqual(f.spawned.at(-1).slice(1), [], "the identity probe sends no arguments (no store is read)");
    assert.deepEqual({ ...resolveCredentialProvider({ dataRoot: f.dataRoot, platform }) }, { file, sha256: sha("bytes-1"), version: "0.4.3", source: "managed" });
  }
});

test("a file that does not answer like rightkit-credentials is refused and nothing is recorded", () => {
  for (const answer of [{ status: 0, stdout: "rightkit-credentials 0.4.3\n" }, { status: 1, stdout: "not json" }, { status: 1, stdout: '{"ok":true}' }, { error: new Error("spawn") }]) {
    const f = fixture({ answer });
    assert.equal(code(() => f.enroll()), "RK_CRED_PROVIDER_IDENTITY");
    assert.equal(fs.existsSync(path.join(f.dataRoot, "credential-provider")), false);
  }
});

test("a version below 0.2.2 or not major.minor.patch is refused; 0.2.2 and later enrol", () => {
  for (const version of ["0.2.1", "0.1.9", "0.4", "latest", ""]) assert.equal(code(() => fixture().enroll({ version })), "RK_CRED_PROVIDER_VERSION", version);
  for (const version of ["0.2.2", "0.10.0", "1.0.0"]) assert.equal(code(() => fixture().enroll({ version })), null, version);
});

test("enrolling is for the host's own platform, with an absolute executable regular file", () => {
  const f = fixture();
  assert.equal(code(() => f.enroll({ hostPlatform: "linux" })), "RK_CRED_PROVIDER_PLATFORM");
  assert.equal(code(() => f.enroll({ hostPlatform: "win32" })), "RK_CRED_PROVIDER_PLATFORM");
  assert.equal(code(() => f.enroll({ binary: "relative/bin" })), "RK_CRED_PROVIDER_PATH");
  assert.equal(code(() => f.enroll({ binary: path.join(f.dataRoot, "missing") })), "RK_CRED_PROVIDER_PATH");
  assert.equal(code(() => f.enroll({ dataRoot: "relative" })), "RK_CRED_PROVIDER_ROOT");
  fs.chmodSync(f.binary, 0o644);
  assert.equal(code(() => f.enroll()), "RK_CRED_PROVIDER_PATH", "not executable");
});

test("an enrolled version is replaced only by rotation", () => {
  const f = fixture();
  f.enroll();
  fs.writeFileSync(f.binary, "bytes-2", { mode: 0o755 });
  assert.equal(code(() => f.enroll()), "RK_CRED_PROVIDER_EXISTS");
  assert.equal(fs.readFileSync(resolveCredentialProvider({ dataRoot: f.dataRoot, platform: "mac" }).file, "utf8"), "bytes-1", "unchanged by the refusal");
  const r = f.enroll({ rotate: true });
  assert.equal(r.rotated, true);
  assert.equal(resolveCredentialProvider({ dataRoot: f.dataRoot, platform: "mac" }).sha256, sha("bytes-2"));
});

test("resolve returns the NEWEST enrolled version", () => {
  const f = fixture();
  f.enroll({ version: "0.4.3" });
  f.enroll({ version: "0.10.0" });
  f.enroll({ version: "0.9.9" });
  assert.equal(resolveCredentialProvider({ dataRoot: f.dataRoot, platform: "mac" }).version, "0.10.0");
});

test("resolve refuses a missing enrolment, a changed file and a missing file, each naming the sanctioned install", () => {
  const f = fixture();
  const none = (() => { try { resolveCredentialProvider({ dataRoot: f.dataRoot, platform: "mac" }); } catch (error) { return error; } })();
  assert.equal(none.code, "RK_CRED_PROVIDER_MISSING");
  assert.ok(none.message.includes(INSTALL), none.message);
  const r = f.enroll();
  fs.writeFileSync(r.providerFile, "tampered");
  const drift = (() => { try { resolveCredentialProvider({ dataRoot: f.dataRoot, platform: "mac" }); } catch (error) { return error; } })();
  assert.equal(drift.code, "RK_CRED_PROVIDER_DIGEST");
  assert.ok(drift.message.includes(sha("tampered")) && drift.message.includes(sha("bytes-1")) && drift.message.includes(INSTALL));
  fs.rmSync(r.providerFile);
  assert.equal(code(() => resolveCredentialProvider({ dataRoot: f.dataRoot, platform: "mac" })), "RK_CRED_PROVIDER_MISSING");
});

test("resolve refuses a linked file and a record that names another path", () => {
  const f = fixture();
  const r = f.enroll();
  const elsewhere = path.join(f.dataRoot, "elsewhere"); fs.writeFileSync(elsewhere, "bytes-1", { mode: 0o755 });
  fs.rmSync(r.providerFile); fs.symlinkSync(elsewhere, r.providerFile);
  assert.equal(code(() => resolveCredentialProvider({ dataRoot: f.dataRoot, platform: "mac" })), "RK_CRED_PROVIDER_RECORD");
  const g = fixture();
  g.enroll();
  const record = path.join(g.dataRoot, "credential-provider", "mac", "record.json");
  const parsed = JSON.parse(fs.readFileSync(record, "utf8")); parsed.providers["0.4.3"].path = path.join(g.dataRoot, "elsewhere");
  fs.writeFileSync(record, JSON.stringify(parsed), { mode: 0o600 });
  assert.equal(code(() => resolveCredentialProvider({ dataRoot: g.dataRoot, platform: "mac" })), "RK_CRED_PROVIDER_RECORD");
});

test("updaterCredentialProvider: the enrolled file on a Mac or Windows host, the PATH name only where no managed data root exists", () => {
  const f = fixture();
  const r = f.enroll();
  assert.deepEqual(updaterCredentialProvider({ env: { RUSTRIGHT_DATA_ROOT: f.dataRoot }, hostPlatform: "darwin" }), { file: r.providerFile, managed: true });
  assert.deepEqual(updaterCredentialProvider({ env: {}, hostPlatform: "linux" }), { file: "rightkit-credentials", managed: false });
  assert.equal(code(() => updaterCredentialProvider({ env: { RUSTRIGHT_DATA_ROOT: tempRoot("cred-empty-") }, hostPlatform: "darwin" })), "RK_CRED_PROVIDER_MISSING");
});

test("execWithUpdaterCredential runs the enrolled provider on a managed host and the bare name on Linux; a missing enrolment refuses before launching", () => {
  const f = fixture();
  const r = f.enroll();
  const calls = [];
  const run = (...args) => { calls.push(args); return { status: 0 }; };
  execWithUpdaterCredential(process.execPath, ["x"], { env: { RUSTRIGHT_DATA_ROOT: f.dataRoot }, hostPlatform: "darwin", account: "u", run });
  assert.equal(calls[0][0], r.providerFile, "an enrolled path, not a PATH lookup");
  execWithUpdaterCredential(process.execPath, ["x"], { env: {}, hostPlatform: "linux", account: "u", run });
  assert.equal(calls[1][0], "rightkit-credentials");
  execWithUpdaterCredential(process.execPath, ["x"], { env: {}, hostPlatform: "darwin", provider: "/explicit/provider", account: "u", run });
  assert.equal(calls[2][0], "/explicit/provider");
  fs.writeFileSync(r.providerFile, "tampered");
  assert.throws(() => execWithUpdaterCredential(process.execPath, [], { env: { RUSTRIGHT_DATA_ROOT: f.dataRoot }, hostPlatform: "darwin", run }), /changed after enrolment/);
  assert.throws(() => execWithUpdaterCredential(process.execPath, [], { env: { RUSTRIGHT_DATA_ROOT: tempRoot("cred-none-") }, hostPlatform: "win32", run }), new RegExp(INSTALL));
  assert.equal(calls.length, 3, "nothing launched after a refusal");
});

test("the Windows store command runs the enrolled provider when one is given", () => {
  const command = protectedSecretStoreCommand("updater-signing-key", "win32", { provider: "D:\\data\\it's\\rightkit-credentials.exe" });
  assert.match(command, /\| & 'D:\\data\\it''s\\rightkit-credentials\.exe' set --namespace 'rightsuite-updater-key'/);
  assert.match(protectedSecretStoreCommand("updater-signing-key", "win32", { env: {} }), /\| & rightkit-credentials\.exe set /, "no enrolment: the bare name");
});

test("the CLI enrols and shows (on a Mac host: the platform is the host's own)", { skip: process.platform !== "darwin" }, () => {
  const f = fixture();
  const out = [], err = [];
  const io = { out: (m) => out.push(m), err: (m) => err.push(m), spawn: f.spawn };
  assert.equal(main(["enroll", "--platform", "mac", "--data-root", f.dataRoot, "--binary", f.binary, "--version", "0.4.3"], io), 0, err.join("\n"));
  assert.match(out[0], /^enrolled rightkit-credentials 0\.4\.3 for mac \(sha256 [0-9a-f]{64}\)/);
  assert.equal(main(["show", "--platform", "mac", "--data-root", f.dataRoot], io), 0);
  assert.equal(JSON.parse(out[1]).providers["0.4.3"].version, "0.4.3");
  assert.equal(main(["enroll", "--platform", "mac", "--data-root", f.dataRoot, "--binary", f.binary, "--version", "0.4.3"], io), 1, "a second enrolment needs --rotate");
  assert.equal(main(["bogus"], io), 2);
});

// ---- the tool route ----------------------------------------------------------------------------------------------------------------
const commit = "a".repeat(40), tree = "b".repeat(40);
function route({ platform = "darwin", version = "0.4.3", answer = IS_CLI } = {}) {
  const root = tempRoot("cred-tool-");
  const data = path.join(root, "data"); fs.mkdirSync(data);
  const target = path.join(root, "checkout", "crates", "rightkit-credentials", "target"); fs.mkdirSync(path.join(target, "release"), { recursive: true });
  const exe = platform === "win32" ? "rightkit-credentials.exe" : "rightkit-credentials";
  const binary = path.join(target, "release", exe);
  const calls = [], outLines = [], errLines = [];
  const hostPlatform = platform;
  const args = {
    selector: "cargo:rightkit-credentials", root: path.join(root, "snap"), commonDir: path.join(root, "common"), commit, tree, executor: { kind: "direct" }, env: { PATH: "/usr/bin" }, platform,
    envExtra: { CARGO_TARGET_DIR: target }, targetDirs: { "crates/rightkit-credentials": target },
    resolve: () => ({ kind: "cargo", name: "rightkit-credentials", version }),
    readKnownRed: () => JSON.stringify({}),
    check: async (tier) => { calls.push(`check:${tier}`); return 0; },
    run: async (argv, options) => {
      calls.push(["cargo", ...argv].join(" "));
      assert.equal(options.cwd, path.join(root, "snap", "crates", "rightkit-credentials"));
      fs.writeFileSync(binary, "built-bytes", { mode: 0o755 });
      return { status: 0, stdout: "", stderr: "" };
    },
    dataRoot: data,
    enroll: () => { throw new Error("the qa-runner enrolment must not run for the credential unit"); },
    enrollProvider: (a) => { calls.push("enroll-provider"); return enrollCredentialProvider({ ...a, spawn: () => answer, hostPlatform }); },
    out: (m) => outLines.push(m), err: (m) => errLines.push(m),
  };
  fs.mkdirSync(args.commonDir, { recursive: true });
  return { args, calls, outLines, errLines, data, binary };
}

test("tool route: class A, the build of the credentials bin, then the credential-provider enrolment (not the qa-runner one); the copy is the built bytes", async () => {
  const s = route();
  assert.equal(await runUnitTool(s.args), 0, s.errLines.join("\n"));
  assert.deepEqual(s.calls, ["check:compile", "cargo build --release --locked --bin rightkit-credentials", "enroll-provider"]);
  const file = path.join(s.data, "credential-provider", "mac", "0.4.3", "rightkit-credentials");
  assert.equal(fs.readFileSync(file, "utf8"), "built-bytes");
  assert.ok(s.outLines.join("\n").includes(`${file} sha256 ${sha("built-bytes")}`));
  assert.equal(resolveCredentialProvider({ dataRoot: s.data, platform: "mac" }).file, file);
});

test("tool route on Windows enrols the .exe for win; a wrong identity or a rotation is refused with the built binary named", async () => {
  const w = route({ platform: "win32" });
  assert.equal(await runUnitTool(w.args), 0, w.errLines.join("\n"));
  assert.ok(fs.existsSync(path.join(w.data, "credential-provider", "win", "0.4.3", "rightkit-credentials.exe")));
  const bad = route({ answer: { status: 0, stdout: "rightkit-credentials 0.4.3" } });
  assert.equal(await runUnitTool(bad.args), 1);
  assert.match(bad.errLines.join("\n"), /RK_CRED_PROVIDER_IDENTITY/);
  const low = route({ version: "0.2.1" });
  assert.equal(await runUnitTool(low.args), 1);
  assert.match(low.errLines.join("\n"), /RK_CRED_PROVIDER_VERSION/);
  const s = route();
  assert.equal(await runUnitTool(s.args), 0);
  s.errLines.length = 0;
  assert.equal(await runUnitTool({ ...s.args, run: async () => { fs.writeFileSync(s.binary, "rebuilt", { mode: 0o755 }); return { status: 0, stdout: "", stderr: "" }; } }), 1);
  const text = s.errLines.join("\n");
  assert.match(text, /RK_CRED_PROVIDER_EXISTS/);
  assert.match(text, /credential-provider\.mjs enroll --binary .* --rotate/);
});
