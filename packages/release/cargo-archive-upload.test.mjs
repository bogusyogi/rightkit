import assert from "node:assert/strict";
import test from "node:test";
import { mkdtempSync, writeFileSync, rmSync, mkdirSync, renameSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { gzipSync } from "node:zlib";
import { publishGatedCrate, crateUploadMetadata } from "./cargo-archive-upload.mjs";
import { digestBytes } from "./publication-artifacts.mjs";
import { runCargoPublish } from "./publish-cargo.mjs";
import { checkPublicationGate } from "./publication-gate.mjs";

const name = "rightkit-fixture", version = "1.0.0";
function archive(manifest = `[package]\nname = "${name}"\nversion = "${version}"\ndescription = "fixture"\nlicense = "MIT"\nreadme = "README.md"\n[dependencies.renamed]\npackage = "actual"\nversion = "1"\noptional = true\ndefault-features = false\n[target.'cfg(windows)'.build-dependencies]\nwin = "2"\n`) {
  const parts = [];
  for (const [file, value] of [["Cargo.toml", manifest], ["README.md", "archive README"]]) {
    const bytes = Buffer.from(value), header = Buffer.alloc(512);
    header.write(`${name}-${version}/${file}`); header.write("0000644\0", 100); header.write("0000000\0", 108); header.write("0000000\0", 116);
    header.write(bytes.length.toString(8).padStart(11, "0") + "\0", 124); header.write("00000000000\0", 136); header.fill(32, 148, 156); header[156] = 48;
    header.write("ustar\0", 257); header.write("00", 263);
    header.write([...header].reduce((sum, byte) => sum + byte, 0).toString(8).padStart(6, "0") + "\0 ", 148);
    parts.push(header, bytes, Buffer.alloc(Math.ceil(bytes.length / 512) * 512 - bytes.length));
  }
  return gzipSync(Buffer.concat([...parts, Buffer.alloc(1024)]));
}
function fixture(t) {
  const root = mkdtempSync(path.join(os.tmpdir(), "rightkit-crate-")); t.after(() => rmSync(root, { recursive: true, force: true }));
  const crateFile = path.join(root, `${name}-${version}.crate`), bytes = archive(); writeFileSync(crateFile, bytes);
  return { root, bytes, options: { name, version, crateFile, evidence: { artifactSha256: digestBytes(bytes) } } };
}

test("crate hash mismatch refuses publication before token or transport", async (t) => {
  const fx = fixture(t); fx.options.evidence.artifactSha256 = "a".repeat(64);
  await assert.rejects(publishGatedCrate(fx.options, { readToken: () => { throw new Error("must not read token"); }, fetch: () => { throw new Error("must not upload"); } }), (error) => error.code === "PUBLICATION_HASH_MISMATCH");
});

test("crate source replacement after hashing refuses publication", async (t) => {
  const fx = fixture(t);
  await assert.rejects(publishGatedCrate(fx.options, {
    readToken: () => { const replacement = path.join(fx.root, "new.crate"); writeFileSync(replacement, fx.bytes); renameSync(replacement, fx.options.crateFile); return "TOKEN"; },
    fetch: () => { throw new Error("must not upload"); },
  }), (error) => error.code === "PUBLICATION_SOURCE_CHANGED");
});

test("crate PUT uses exact hashed archive with archive-derived metadata", async (t) => {
  const fx = fixture(t);
  const result = await publishGatedCrate(fx.options, {
    readToken: () => "TOKEN",
    fetch: async (url, options) => {
      writeFileSync(fx.options.crateFile, "replacement after final check");
      assert.equal(url, "https://crates.io/api/v1/crates/new"); assert.equal(options.method, "PUT");
      const body = options.body, metadataLength = body.readUInt32LE(0), metadata = JSON.parse(body.subarray(4, 4 + metadataLength));
      const crateLength = body.readUInt32LE(4 + metadataLength), uploaded = body.subarray(8 + metadataLength);
      assert.equal(crateLength, fx.bytes.length); assert.deepEqual(uploaded, fx.bytes); assert.equal(digestBytes(uploaded), fx.options.evidence.artifactSha256);
      assert.equal(metadata.readme, "archive README"); assert.equal(metadata.name, name); assert.equal(metadata.vers, version);
      assert.equal(metadata.deps[0].name, "actual"); assert.equal(metadata.deps[0].explicit_name_in_toml, "renamed");
      assert.equal(metadata.deps[1].target, "cfg(windows)"); assert.equal(metadata.deps[1].kind, "build");
      return { ok: true, status: 200, json: async () => ({ warnings: {} }) };
    },
  });
  assert.equal(result.sha256, fx.options.evidence.artifactSha256);
});

test("crate identity mismatch & registry rejection fail closed", async (t) => {
  const fx = fixture(t);
  assert.throws(() => crateUploadMetadata(fx.bytes, name, "2.0.0"), /Cargo.toml missing/);
  await assert.rejects(publishGatedCrate(fx.options, { readToken: () => "TOKEN", fetch: async () => ({ ok: true, status: 200, json: async () => ({ errors: [{ detail: "TOKEN must never print" }] }) }) }), (error) => error.code === "PUBLICATION_UPLOAD_FAILED" && !error.message.includes("TOKEN"));
});

test("publication gate compares actual artifact buffer to evidence", () => {
  const commit = "a".repeat(40), bytes = archive();
  const options = { kind: "cargo", name, version, packagePath: "/fixture", evidencePath: "/evidence", artifactBytes: bytes,
    git: (args) => args[0] === "rev-parse" ? commit : "",
    readFile: () => JSON.stringify({ schema: 1, kind: "cargo", name, version, commit, artifactSha256: digestBytes(bytes), runs: ["darwin", "win32"].map((os) => ({ host: { os }, commit, command: "node --test", passed: 1, failed: 0, operator: "test", timestamp: "2026-10-08" })) }),
  };
  assert.equal(checkPublicationGate(options).ok, true);
  assert.equal(checkPublicationGate({ ...options, artifactBytes: Buffer.from("changed") }).ok, false);
});

test("Cargo publisher uses generated target archive & never executes real cargo publish", async (t) => {
  const fx = fixture(t), crateDir = path.join(fx.root, "crates", name), target = path.join(fx.root, "generated-target");
  mkdirSync(crateDir, { recursive: true }); writeFileSync(path.join(crateDir, "Cargo.toml"), `[package]\nname = "${name}"\nversion = "${version}"\n`);
  mkdirSync(path.join(target, "package"), { recursive: true }); const crateFile = path.join(target, "package", `${name}-${version}.crate`); writeFileSync(crateFile, fx.bytes);
  const evidence = fx.options.evidence;
  let requests = 0;
  await runCargoPublish({ workspaceRoot: fx.root, crate: name }, (_cmd, args) => {
    assert.ok(args[0] !== "publish" || args.includes("--dry-run"));
    if (args[0] === "metadata") return { status: 0, stdout: JSON.stringify({ target_directory: target }) };
    return { status: 0, stdout: args.includes("--list") ? "Cargo.toml\nsrc/lib.rs\n" : "" };
  }, {
    enforceGate: () => ({ ok: true, evidence }), readToken: () => "TOKEN",
    fetch: async (_url, options) => { requests++; assert.ok(options.body.includes(fx.bytes)); return { ok: true, status: 200, json: async () => ({}) }; },
  });
  assert.equal(requests, 1);
});
