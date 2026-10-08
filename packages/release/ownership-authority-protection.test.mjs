import assert from "node:assert/strict";
import { createHash, generateKeyPairSync, sign } from "node:crypto";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, symlinkSync, writeFileSync, realpathSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";
import { spawnSync } from "./background-process.mjs";
import { __testOnly as approval, canonicalJson } from "./ownership-approval.mjs";
import { __testOnly as command } from "./ownership-key-command.mjs";
import { keyFingerprint } from "./ownership-key-log.mjs";

test("prefixed & uppercase key IDs normalize for revocation, enrollment & authority", () => {
  const keys = Object.fromEntries(["windows", "macos"].map((host) => {
    const pair = generateKeyPairSync("ec", { namedCurve: "prime256v1" });
    const publicKeyPem = pair.publicKey.export({ type: "spki", format: "pem" });
    return [host, { keyId: keyFingerprint(publicKeyPem), publicKeyPem, privateKey: pair.privateKey, platform: host }];
  }));
  for (const host of ["windows", "macos"]) {
    const prefix = host === "windows" ? "win-" : "mac-";
    const payload = { purpose: "normalization regression" };
    const record = { payload, signature: { host, keyId: `${prefix}${keys[host].keyId.toUpperCase()}`, value: sign("sha256", Buffer.from(canonicalJson(payload)), keys[host].privateKey).toString("base64") } };
    const legacy = { hosts: keys, revokedKeyIds: [] };
    const authority = { rootFingerprint: keys.windows.keyId, ownershipKeys: Object.values(keys), revokedKeyIds: [] };
    for (const trust of [legacy, authority]) {
      assert.deepEqual(approval.signatureProblems(record, trust), []);
      for (const revokedId of [keys[host].keyId, `${prefix}${keys[host].keyId.toUpperCase()}`]) {
        assert.deepEqual(approval.signatureProblems(record, { ...trust, revokedKeyIds: [revokedId] }), ["signing key is revoked"]);
      }
    }
    const duplicate = { ...legacy, hosts: { windows: keys.windows, macos: { ...keys.windows, keyId: `mac-${keys.windows.keyId.toUpperCase()}` } } };
    const windowsRecord = { payload, signature: { host: "windows", keyId: `win-${keys.windows.keyId}`, value: sign("sha256", Buffer.from(canonicalJson(payload)), keys.windows.privateKey).toString("base64") } };
    assert.match(approval.signatureProblems(windowsRecord, duplicate).join(" "), /distinct Windows and macOS/);
  }
});

test("macOS protectedLocation rejects every non-root mutating ACE including writesecurity", () => {
  const inspect = () => ({ isSymbolicLink: () => false });
  const stat = () => ({ uid: 0, mode: 0o755 });
  for (const permission of ["writesecurity", "chown", "write", "delete", "append", "writeattr", "writeextattr", "delete_child", "add_file", "add_subdirectory"]) {
    for (const principal of ["user:agent", "group:admin", "user:rootish"]) {
      const result = approval.protectedLocation(path.resolve("trust.json"), "darwin", { inspect, stat, runner: () => ({ status: 0, stdout: `file\n 0: ${principal} allow ${permission}\n` }) });
      assert.equal(result.ok, false, `${principal} ${permission}`);
      assert.match(result.problems.join(" "), /non-root principal/);
    }
    assert.equal(approval.protectedLocation(path.resolve("trust.json"), "darwin", { inspect, stat, runner: () => ({ status: 0, stdout: `file\n 0: user:root allow ${permission}\n` }) }).ok, true);
  }
});

const shell = process.platform === "win32" ? "C:\\Program Files\\Git\\bin\\bash.exe" : "/bin/bash";
const awk = process.platform === "win32" ? "C:\\Program Files\\Git\\usr\\bin\\awk.exe" : "/usr/bin/awk";
const adapter = readFileSync(new URL("../../ownership-approval/adapters/private-local.mjs", import.meta.url), "utf8");
test("every macOS shell permission check parses octal: 0755 passes, 0775 fails", () => {
  assert.equal(adapter.includes("$(( mode &"), false);
  const expressions = [...adapter.matchAll(/\$\(\( 0\$mode & (022|077) \)\)/g)];
  assert.equal(expressions.length, 5);
  for (const [, mask] of expressions) {
    for (const mode of mask === "022" ? ["755", "0755", "775", "0775"] : ["600", "0600", "644", "0644"]) {
      const result = spawnSync(shell, ["-c", `mode=${mode}; test $(( 0$mode & ${mask} )) -eq 0`], { encoding: "utf8", windowsHide: true });
      assert.equal(result.status, ["755", "0755", "600", "0600"].includes(mode) ? 0 : 1, `${mode} & ${mask}: ${result.stderr}`);
    }
  }
});

test("every adapter awk mutating-ACL audit rejects non-root writesecurity & chown", () => {
  const audits = [...adapter.matchAll(/'(NR>1[^']+END \{exit bad\})'/g)].map((match) => match[1]);
  assert.equal(audits.length, 6);
  for (const script of audits) {
    for (const principal of ["user:agent", "user:svc", "group:admin"]) {
      for (const permission of ["writesecurity", "chown"]) {
        const result = spawnSync(awk, ["-v", "svc=svc", script], { encoding: "utf8", windowsHide: true, input: `file\n 0: ${principal} allow ${permission}\n` });
        assert.equal(result.status, 1, `${principal} ${permission}: ${result.stderr}`);
      }
    }
    const root = spawnSync(awk, ["-v", "svc=svc", script], { encoding: "utf8", windowsHide: true, input: "file\n 0: user:root allow writesecurity,chown\n" });
    assert.equal(root.status, 0, root.stderr);
  }
});

function checkoutFixture() {
  const directory = mkdtempSync(path.join(tmpdir(), "rk-elevation-"));
  const root = path.join(directory, "checkout");
  mkdirSync(root, { recursive: true });
  for (const name of ["ownership-key-command.mjs", "ownership-key-trust.mjs", "ownership-key-authority.mjs", "ownership-repositories.json"]) writeFileSync(path.join(root, name), "checkout fixture\n");
  const requestPath = path.join(directory, "request.json");
  writeFileSync(requestPath, JSON.stringify({ schema: 1, fixture: true }));
  const digest = createHash("sha256").update(readFileSync(requestPath)).digest("hex");
  return { directory, root, requestPath, digest, options: { modulePath: path.join(root, "ownership-key-command.mjs"), nodeExe: process.execPath, audit: () => ({ ok: true }) } };
}
const typedRefusal = (error) => error.code === "RK_OWNERSHIP_ELEVATION_UNPROTECTED" && error.type === error.code;
test("elevation refuses missing request, malformed or mismatched digest & linked sibling before launch", (t) => {
  const fixture = checkoutFixture();
  let calls = 0;
  const run = () => { calls++; return { status: 0 }; };
  const elevate = (requestPath = fixture.requestPath, digest = fixture.digest, options = fixture.options) => command.elevate(run, requestPath, digest, options);
  try {
    assert.throws(() => elevate(path.join(fixture.directory, "missing.json")), typedRefusal);
    assert.throws(() => elevate(fixture.requestPath, ""), typedRefusal);
    assert.throws(() => elevate(fixture.requestPath, "a".repeat(64)), (error) => typedRefusal(error) && /SHA-256/.test(error.message));
    const linked = path.join(fixture.root, "ownership-key-trust.mjs");
    const original = `${linked}.original`;
    rmSync(linked);
    writeFileSync(original, "linked target\n");
    try { symlinkSync(original, linked, "file"); }
    catch { t.skip("file symlink creation unavailable"); return; }
    assert.throws(() => elevate(), (error) => typedRefusal(error) && /non-linked file/.test(error.message));
    assert.equal(calls, 0);
  } finally { rmSync(fixture.directory, { recursive: true, force: true }); }
});

test("checkout elevation dispatches both trust updater & authority wrapper", () => {
  const fixture = checkoutFixture();
  const calls = [];
  try {
    command.elevate((exe, args, options) => { calls.push({ exe, args, options }); return { status: 0 }; }, fixture.requestPath, fixture.digest, fixture.options);
    const elevation = calls.find(({ exe, args }) => exe === "/usr/bin/sudo" ? args[0] === "-k" : /powershell\.exe$/i.test(exe));
    assert.ok(elevation);
    if (process.platform === "win32") {
      const parent = Buffer.from(elevation.args.at(-1), "base64").toString("utf16le");
      assert.ok(parent.includes("$env:NODE_OPTIONS=$null; $env:NODE_PATH=$null;"));
      const encoded = parent.match(/-EncodedCommand ([A-Za-z0-9+/=]+)/)[1];
      const child = Buffer.from(encoded, "base64").toString("utf16le");
      assert.ok(child.includes(path.join(fixture.root, "ownership-key-trust.mjs")));
      assert.ok(child.includes(path.join(fixture.root, "ownership-key-authority.mjs")));
      assert.ok(child.includes("--trust-script"));
    } else {
      assert.ok(elevation.args.includes(path.join(realpathSync(fixture.root), "ownership-key-trust.mjs")));
      assert.ok(elevation.args.includes(path.join(realpathSync(fixture.root), "ownership-key-authority.mjs")));
      assert.equal(calls.at(-1).args[0], "-K");
    }
  } finally { rmSync(fixture.directory, { recursive: true, force: true }); }
});
