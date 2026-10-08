import assert from "node:assert/strict";
import { generateKeyPairSync, sign } from "node:crypto";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";
import { spawnSync } from "./background-process.mjs";
import { __testOnly as approval, canonicalJson } from "./ownership-approval.mjs";
import { __testOnly as command } from "./ownership-key-command.mjs";
import { keyFingerprint } from "./ownership-key-log.mjs";
import { createProtectedFileManifest } from "../../ownership-approval/adapters/package-manifest.mjs";

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

function installedFixture() {
  const directory = mkdtempSync(path.join(tmpdir(), "rk-elevation-"));
  const root = path.join(directory, "runtime-current");
  for (const dir of ["release", "runtime", "build-control-clients/src", "ownership/adapters"]) mkdirSync(path.join(root, dir), { recursive: true });
  for (const relative of ["release/ownership-key-command.mjs", "release/ownership-key-trust.mjs", "release/ownership-key-authority.mjs", "runtime/node.exe", "build-control-clients/src/rightkit.mjs"]) writeFileSync(path.join(root, relative), "installer-owned fixture\n");
  const manifestPath = path.join(root, "ownership/adapters/adapter-files.json");
  writeFileSync(manifestPath, JSON.stringify(createProtectedFileManifest(root)));
  return { directory, root, manifestPath, options: { modulePath: path.join(root, "release/ownership-key-command.mjs"), nodeExe: path.join(root, "runtime/node.exe"), runtimeClient: path.join(root, "build-control-clients/src/rightkit.mjs"), audit: () => ({ ok: true }) } };
}
const typedRefusal = (error) => error.code === "RK_OWNERSHIP_ELEVATION_UNPROTECTED" && error.type === error.code;
test("elevation refuses development checkout, missing manifest, missing digest & digest mismatch before launch", () => {
  const fixture = installedFixture();
  let calls = 0;
  const run = () => { calls++; return { status: 0 }; };
  const elevate = (options = fixture.options) => command.elevate(run, path.join(fixture.directory, "request.json"), "a".repeat(64), options);
  try {
    assert.throws(() => elevate({ ...fixture.options, modulePath: fileURLToPath(new URL("./ownership-key-command.mjs", import.meta.url)) }), (error) => typedRefusal(error) && /outside installer-owned/.test(error.message));
    const manifest = readFileSync(fixture.manifestPath);
    rmSync(fixture.manifestPath);
    assert.throws(() => elevate(), typedRefusal);
    writeFileSync(fixture.manifestPath, manifest);
    const changed = JSON.parse(manifest);
    changed.files = changed.files.filter((entry) => entry.path !== "release/ownership-key-trust.mjs");
    writeFileSync(fixture.manifestPath, JSON.stringify(changed));
    assert.throws(() => elevate(), typedRefusal);
    writeFileSync(fixture.manifestPath, manifest);
    writeFileSync(path.join(fixture.root, "release/ownership-key-trust.mjs"), "mutated elevated code\n");
    assert.throws(() => elevate(), (error) => typedRefusal(error) && /ownership-key-trust\.mjs/.test(error.message));
    assert.equal(calls, 0);
  } finally { rmSync(fixture.directory, { recursive: true, force: true }); }
});

test("verified installed elevation passes absolute trust path & checkpoint wrapper", () => {
  const fixture = installedFixture();
  const calls = [];
  try {
    command.elevate((exe, args) => { calls.push({ exe, args }); return { status: 0 }; }, path.join(fixture.directory, "request.json"), "a".repeat(64), fixture.options);
    if (process.platform === "win32") {
      const parent = Buffer.from(calls[0].args.at(-1), "base64").toString("utf16le");
      assert.ok(parent.includes("$env:NODE_OPTIONS=$null; $env:NODE_PATH=$null;"));
      const encoded = parent.match(/-EncodedCommand ([A-Za-z0-9+/=]+)/)[1];
      const child = Buffer.from(encoded, "base64").toString("utf16le");
      assert.ok(child.includes(path.join(fixture.root, "release/ownership-key-trust.mjs")));
      assert.ok(child.includes(path.join(fixture.root, "release/ownership-key-authority.mjs")));
      assert.ok(child.includes("--trust-script"));
    } else {
      assert.ok(calls[0].args.includes(path.join(fixture.root, "release/ownership-key-trust.mjs")));
      assert.equal(calls.at(-1).args[0], "-K");
    }
  } finally { rmSync(fixture.directory, { recursive: true, force: true }); }
});
