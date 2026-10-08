import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "./background-process.mjs";
import { fileURLToPath } from "node:url";
import test from "node:test";
import { digestBytes } from "./publication-artifacts.mjs";
import { runLargeUpload } from "./upload-large.mjs";
import { createCredentialStore } from "./protected-credentials.mjs";
import { readFileSync } from "node:fs";

const uploader = fileURLToPath(new URL("./upload-large.mjs", import.meta.url));

function fixtureFile() {
  const dir = mkdtempSync(path.join(os.tmpdir(), "right-upload-test-"));
  const file = path.join(dir, "Artifact.exe");
  writeFileSync(file, "artifact");
  return file;
}

function evidenceFor(file) {
  const evidence = `${file}.evidence.json`;
  writeFileSync(evidence, JSON.stringify({ artifactSha256: digestBytes(readFileSync(file)) }));
  return evidence;
}
function run(bucket) {
  const file = fixtureFile();
  return spawnSync(process.execPath, [uploader, file, "fixture/updates/windows/current/Artifact.exe", bucket, "--evidence", evidenceFor(file)], {
    encoding: "utf8",
    env: { ...process.env, RIGHT_RELEASE_UPLOAD_DRY_RUN: "1" },
  });
}

test("uploads public artifacts through wrangler remote R2 downloads bucket", () => {
  const result = run("public");
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /wrangler r2 object put rightapps-downloads\/fixture\/updates\/windows\/current\/Artifact\.exe/);
  assert.match(result.stdout, /--remote/);
  assert.doesNotMatch(result.stdout, /workers\.dev|multipart|heardright-models/i);
});

test("uploads private artifacts through wrangler remote R2 updates bucket", () => {
  const result = run("private");
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /wrangler r2 object put rightapps-updates\/fixture\/updates\/windows\/current\/Artifact\.exe/);
  assert.match(result.stdout, /--remote/);
});

test("refuses unknown bucket aliases", () => {
  const result = run("random");
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /expected public\|private/);
});

test("resolves a real package runner for wrangler on this machine", () => {
  const result = run("public");
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /^runner: pnpm dlx$/m);
});

test("fails loudly when no package runner exists instead of skipping the upload", () => {
  const file = fixtureFile();
  const result = spawnSync(
    process.execPath,
    [uploader, file, "fixture/installers/windows/current/Artifact.exe", "public", "--evidence", evidenceFor(file)],
    {
      encoding: "utf8",
      env: {
        ...process.env,
        RIGHT_RELEASE_UPLOAD_DRY_RUN: "1",
        RIGHT_RELEASE_WRANGLER_RUNNER: "",
        PATH: path.join(os.tmpdir(), "right-upload-empty-path"),
        Path: path.join(os.tmpdir(), "right-upload-empty-path"),
        PATHEXT: process.env.PATHEXT ?? "",
      },
    },
  );
  assert.notEqual(result.status, 0, "a box with no runner must not report a successful upload");
  assert.match(result.stderr, /no package runner found/);
});

for (const entry of ["upload-large", "pub-upload"]) {
  test(`${entry}: unsealed CLI input refused`, () => {
    const result = spawnSync(process.execPath, [path.join(path.dirname(uploader), `${entry}.mjs`), fixtureFile(), "fixture/file", "public"], { encoding: "utf8" });
    assert.notEqual(result.status, 0); assert.match(result.stderr, /seal or hash evidence is required/);
  });
  for (const mode of ["mismatch", "swap", "happy", "missing readback", "wrong readback"]) {
    test(`${entry}: ${mode} with bounded-file transport mocks`, () => {
      const file = fixtureFile(), evidence = evidenceFor(file), calls = [];
      if (mode === "mismatch") writeFileSync(file, "tampered");
      let remote;
      const run = (_cmd, args) => {
        calls.push(args);
        if (args[0] === "--version") return { status: 0 };
        if (args.includes("put")) {
          if (mode === "swap") writeFileSync(file, "swapped during upload");
          remote = readFileSync(file); return { status: 0 };
        }
        if (args.includes("get")) {
          if (mode === "missing readback") return { status: 1 };
          writeFileSync(args[args.indexOf("--file") + 1], mode === "wrong readback" ? "wrong" : remote);
          return { status: 0 };
        }
        throw new Error(`unexpected call ${args}`);
      };
      const credentialStore = createCredentialStore({ env: {}, platform: "darwin", readKeychain: () => "fake-store-token" });
      const invoke = () => runLargeUpload([file, "fixture/file", "private", "--evidence", evidence], { run, env: {}, credentialStore });
      if (mode === "happy") { assert.equal(invoke().sha256, digestBytes(Buffer.from("artifact"))); assert.equal(calls.length, 3); }
      else { assert.throws(invoke, mode === "mismatch" ? /HASH_MISMATCH/ : /remote key rightapps-updates\/fixture\/file/); }
      if (mode === "mismatch") assert.equal(calls.length, 0);
      assert.equal(calls.some((args) => args.includes("delete")), false);
    });
  }
}
