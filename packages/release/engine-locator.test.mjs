import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";
import { ENGINES, ENGINE_TEAM_IDENTIFIER, EngineLocateError, engineRoute, locateEngine } from "./engine-locator.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));

test("routes are fixed per platform and engine", () => {
  assert.equal(engineRoute("cutright", { platform: "darwin", home: "/Users/a" }), "/Users/a/Library/Application Support/Orthic Labs/CutRight/current/bin/cutright");
  assert.equal(engineRoute("genright", { platform: "win32", localAppData: "C:\\Users\\a\\AppData\\Local" }), "C:\\Users\\a\\AppData\\Local\\Orthic Labs\\GenRight\\current\\bin\\genright.exe");
  assert.throws(() => engineRoute("other", { platform: "darwin" }), (error) => error.code === "ENGINE_UNKNOWN");
  assert.throws(() => engineRoute("cutright", { platform: "linux" }), (error) => error.code === "ENGINE_PLATFORM_UNSUPPORTED");
  assert.throws(() => engineRoute("cutright", { platform: "win32", localAppData: "" }), (error) => error.code === "ENGINE_ROUTE_UNAVAILABLE");
});

test("no environment override: the locator never reads an engine variable or takes a path option", () => {
  const source = readFileSync(path.join(here, "engine-locator.mjs"), "utf8");
  assert.doesNotMatch(source, /POSTRIGHT|_ENGINE\b/);
  assert.doesNotMatch(source, /process\.env\.(?!LOCALAPPDATA)/);
  const previous = process.env.POSTRIGHT_CUTRIGHT_ENGINE;
  process.env.POSTRIGHT_CUTRIGHT_ENGINE = "/tmp/evil";
  try { assert.notEqual(engineRoute("cutright", { platform: "darwin", home: "/Users/a" }), "/tmp/evil"); } finally {
    if (previous === undefined) delete process.env.POSTRIGHT_CUTRIGHT_ENGINE; else process.env.POSTRIGHT_CUTRIGHT_ENGINE = previous;
  }
});

function installed(content = "engine bytes") {
  const root = mkdtempSync(path.join(os.tmpdir(), "engine-locator-"));
  const file = path.join(root, "cutright");
  writeFileSync(file, content);
  return { root, file, sha256: createHash("sha256").update(content).digest("hex") };
}

test("returns the verified path and sha256 when the signature check passes", () => {
  const f = installed();
  try {
    const seen = [];
    const result = locateEngine("cutright", { platform: "darwin", route: f.file, verifySignature: (file, spec) => seen.push([file, spec.identifier]), expectedSha256: f.sha256 });
    assert.deepEqual(result, { engine: "cutright", path: f.file, sha256: f.sha256, platform: "darwin" });
    assert.deepEqual(seen, [[f.file, ENGINES.cutright.identifier]]);
  } finally { rmSync(f.root, { recursive: true, force: true }); }
});

test("refuses a missing engine, a failed signature check and a pinned hash mismatch", () => {
  const f = installed();
  try {
    assert.throws(() => locateEngine("cutright", { platform: "darwin", route: path.join(f.root, "absent"), verifySignature: () => {} }), (error) => error.code === "ENGINE_NOT_INSTALLED");
    assert.throws(() => locateEngine("cutright", { platform: "darwin", route: f.file, verifySignature: () => { throw new EngineLocateError("ENGINE_SIGNATURE_INVALID", "bad"); } }), (error) => error.code === "ENGINE_SIGNATURE_INVALID");
    assert.throws(() => locateEngine("cutright", { platform: "darwin", route: f.file, verifySignature: () => {}, expectedSha256: "0".repeat(64) }), (error) => error.code === "ENGINE_HASH_MISMATCH");
    assert.throws(() => locateEngine("cutright", { platform: "darwin", route: f.root, verifySignature: () => {} }), (error) => error.code === "ENGINE_NOT_REGULAR_FILE");
  } finally { rmSync(f.root, { recursive: true, force: true }); }
});

test("a symlink at the route is refused before any signature check", (t) => {
  const f = installed();
  try {
    const link = path.join(f.root, "link");
    try { symlinkSync(f.file, link); } catch { t.skip("symlinks unavailable"); return; }
    let verified = false;
    assert.throws(() => locateEngine("cutright", { platform: "darwin", route: link, verifySignature: () => { verified = true; } }), (error) => error.code === "ENGINE_NOT_REGULAR_FILE");
    assert.equal(verified, false);
  } finally { rmSync(f.root, { recursive: true, force: true }); }
});

test("macOS verification pins the Team ID, Identifier and hardened runtime, and refuses ad hoc", () => {
  const f = installed();
  try {
    const display = (team, identifier, flags = "0x10000(runtime)") => `Identifier=${identifier}\nCodeDirectory v=20500 flags=${flags}\nTeamIdentifier=${team}\n`;
    const runWith = (text, strictStatus = 0) => (command, args) => (args.includes("--verify")
      ? { status: strictStatus, stdout: "", stderr: strictStatus ? "invalid" : "" }
      : { status: 0, stdout: "", stderr: text });
    const options = { platform: "darwin", route: f.file };
    assert.equal(locateEngine("cutright", { ...options, run: runWith(display(ENGINE_TEAM_IDENTIFIER, ENGINES.cutright.identifier)) }).path, f.file);
    assert.throws(() => locateEngine("cutright", { ...options, run: runWith(display("OTHERTEAM1", ENGINES.cutright.identifier)) }), (error) => error.code === "ENGINE_IDENTITY_MISMATCH");
    assert.throws(() => locateEngine("cutright", { ...options, run: runWith(display(ENGINE_TEAM_IDENTIFIER, "com.evil.engine")) }), (error) => error.code === "ENGINE_IDENTITY_MISMATCH");
    assert.throws(() => locateEngine("cutright", { ...options, run: runWith(display(ENGINE_TEAM_IDENTIFIER, ENGINES.cutright.identifier, "0x2(adhoc)")) }), (error) => error.code === "ENGINE_IDENTITY_MISMATCH");
    assert.throws(() => locateEngine("cutright", { ...options, run: runWith("", 1) }), (error) => error.code === "ENGINE_SIGNATURE_INVALID");
  } finally { rmSync(f.root, { recursive: true, force: true }); }
});

test("Windows verification needs a Valid Authenticode status and the Orthic Labs signer organisation", () => {
  const f = installed();
  try {
    const runWith = (status, subject) => () => ({ status: 0, stdout: `${JSON.stringify({ status, subject })}\n`, stderr: "" });
    const options = { platform: "win32", route: f.file };
    assert.equal(locateEngine("genright", { ...options, run: runWith("Valid", "CN=Orthic Labs, O=Orthic Labs, C=IN") }).engine, "genright");
    assert.throws(() => locateEngine("genright", { ...options, run: runWith("NotSigned", "") }), (error) => error.code === "ENGINE_SIGNATURE_INVALID");
    assert.throws(() => locateEngine("genright", { ...options, run: runWith("Valid", "CN=Evil, O=Evil Corp") }), (error) => error.code === "ENGINE_IDENTITY_MISMATCH");
  } finally { rmSync(f.root, { recursive: true, force: true }); }
});
