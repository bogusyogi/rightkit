// sign-windows.mjs keeps its CLI and exports the signer and the verifier as functions (rightkit#1303): the gate's in-process signing steps reuse them, so
// there is one Authenticode signer. Fakes only: no signtool, no Azure Artifact Signing, no network.
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { spawnSync } from "./background-process.mjs";
import { fileURLToPath } from "node:url";
import { TOKEN_UNAVAILABLE_CODE, WINDOWS_SIGNER_SUBJECT, WindowsSigningError, WindowsSigningTokenError, assertWindowsVerification, main, signWindowsFiles, verifyWindowsFiles, windowsSigningContext } from "./sign-windows.mjs";

const SIGNTOOL = "C:\\tools\\signtool.exe";
const DLIB = "C:\\tools\\Azure.CodeSigning.Dlib.dll";
const REPORT = (file, issuer = "Damned Ventures LLC", extra = "The signature is timestamped: Oct 30 2026") => `File: ${file}\nSigning Certificate Chain:\n    Issued to: Microsoft ID Verified Code Signing PCA\n        Issued to: ${issuer}\nSuccessfully verified: ${file}\n${extra}\n`;

function setup(t, over = {}) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "rk-signwin-"));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const exe = path.join(dir, "app.exe");
  fs.writeFileSync(exe, "MZ-unsigned");
  const calls = [];
  const tokenCalls = [];
  const known = new Set([SIGNTOOL, DLIB, exe, dir]);
  const run = over.run ?? ((cmd, args) => {
    if (args[0] === "/d" && /az account get-access-token/.test(args[2])) {
      tokenCalls.push(args[2]);
      return over.token ?? { status: 0, stdout: "2099-01-01 00:00:00.000000\n", stderr: "" };
    }
    calls.push([path.win32.basename(cmd), ...args]);
    if (args[0] === "sign") { fs.appendFileSync(args.at(-1), "|signed"); return { status: 0, stdout: "Successfully signed\n", stderr: "" }; }
    if (args[0] === "verify") return { status: 0, stdout: REPORT(args.at(-1), over.issuer), stderr: "" };
    return { status: 1, stdout: "", stderr: "?" };
  });
  const ctx = windowsSigningContext({
    processEnv: { AZURE_SIGNTOOL_PATH: SIGNTOOL, AZURE_CODESIGN_DLIB_PATH: DLIB, AZURE_ARTIFACT_SIGNING_ENDPOINT: "https://eus.codesigning.azure.net", AZURE_ARTIFACT_SIGNING_ACCOUNT: "acct", AZURE_ARTIFACT_SIGNING_PROFILE: "prof", PATH: "C:\\x", ...(over.processEnv ?? {}) },
    run, exists: (file) => known.has(file) && !(over.missing ?? []).includes(file), platform: over.platform ?? "win32", userEnvReader: () => "", quiet: true,
  });
  return { dir, exe, calls, tokenCalls, ctx };
}

test("the signer subject is the identity, never a thumbprint", () => {
  assert.equal(WINDOWS_SIGNER_SUBJECT, "CN=Damned Ventures LLC");
  assert.deepEqual(assertWindowsVerification("f", REPORT("f")), { authenticode: "Valid", subject: "CN=Damned Ventures LLC", timestampPresent: true });
  assert.throws(() => assertWindowsVerification("f", REPORT("f", "Someone Else")), /subject must be Damned Ventures LLC/);
  assert.throws(() => assertWindowsVerification("f", REPORT("f", "Damned Ventures LLC Fake")), /subject must be/);
  assert.throws(() => assertWindowsVerification("f", "Issued to: Damned Ventures LLC\nnot valid\ntimestamp"), /did not report Valid/);
  assert.throws(() => assertWindowsVerification("f", "Successfully verified: f\nIssued to: Damned Ventures LLC\n"), /timestamp is missing/);
});

test("signWindowsFiles signs each file with Azure Artifact Signing, verifies it, and returns evidence; a receipt is optional", (t) => {
  const s = setup(t);
  const receipt = path.join(s.dir, "r", "receipt.json");
  const evidence = signWindowsFiles([s.exe], { ctx: s.ctx, receiptPath: receipt });
  assert.equal(evidence.length, 1);
  assert.equal(evidence[0].file, s.exe);
  assert.equal(evidence[0].subject, "CN=Damned Ventures LLC");
  assert.equal(evidence[0].authenticode, "Valid");
  assert.notEqual(evidence[0].before.sha256, evidence[0].after.sha256);
  assert.deepEqual(s.calls.map((c) => c[1]), ["sign", "verify"]);
  const signCall = s.calls[0];
  assert.ok(signCall.includes("/dlib") && signCall.includes(DLIB) && signCall.includes("/tr") && signCall.includes("/dmdf"));
  assert.deepEqual(JSON.parse(fs.readFileSync(receipt, "utf8")), { schema: 1, files: JSON.parse(JSON.stringify(evidence)) });
  const verified = verifyWindowsFiles([s.exe], { ctx: s.ctx });
  assert.equal(verified[0].subject, "CN=Damned Ventures LLC");
});

test("the signer refuses the wrong subject, a non-Windows host, a missing file, a missing tool and a failing tool", (t) => {
  const wrong = setup(t, { issuer: "Other LLC" });
  assert.throws(() => signWindowsFiles([wrong.exe], { ctx: wrong.ctx }), /subject must be/);
  const mac = setup(t, { platform: "darwin" });
  assert.throws(() => signWindowsFiles([mac.exe], { ctx: mac.ctx }), /must run on Windows/);
  const missingFile = setup(t);
  assert.throws(() => signWindowsFiles([path.join(missingFile.dir, "none.exe")], { ctx: missingFile.ctx }), /missing file/);
  const noTool = setup(t, { missing: [SIGNTOOL] });
  assert.throws(() => signWindowsFiles([noTool.exe], { ctx: noTool.ctx }), /signtool\.exe not found/);
  const noDlib = setup(t, { missing: [DLIB] });
  assert.throws(() => signWindowsFiles([noDlib.exe], { ctx: noDlib.ctx }), /Azure\.CodeSigning\.Dlib\.dll not found/);
  const noMetadata = setup(t, { processEnv: { AZURE_ARTIFACT_SIGNING_ENDPOINT: "", AZURE_ARTIFACT_SIGNING_ACCOUNT: "" } });
  assert.throws(() => signWindowsFiles([noMetadata.exe], { ctx: noMetadata.ctx }), /set AZURE_ARTIFACT_SIGNING_ENDPOINT/);
  const failing = setup(t, { run: (cmd, args) => (args[0] === "/d" ? { status: 0, stdout: "2099-01-01 00:00:00\n", stderr: "" } : { status: 3, stdout: "", stderr: "SignTool Error" }) });
  try { signWindowsFiles([failing.exe], { ctx: failing.ctx }); assert.fail("expected a failure"); } catch (error) { assert.ok(error instanceof WindowsSigningError); assert.equal(error.exitCode, 3); }
});

test("the CLI is unchanged: usage, dry run, non-Windows refusal and exit codes", () => {
  const cli = fileURLToPath(new URL("./sign-windows.mjs", import.meta.url));
  const run = (...args) => spawnSync(process.execPath, [cli, ...args], { encoding: "utf8" });
  const usage = run();
  assert.equal(usage.status, 1);
  assert.match(usage.stderr, /right-sign-windows: usage: node sign-windows\.mjs/);
  const dry = run("--dry-run", "a.exe");
  assert.equal(dry.status, 0);
  assert.match(dry.stdout, /^dry-run: Azure Artifact Signing .*a\.exe/);
  assert.match(run("--dry-run", "--verify-only", "a.exe").stdout, /^dry-run: Authenticode verification/);
  assert.match(run("--receipt").stderr, /usage|--receipt requires a path/);
  if (process.platform !== "win32") {
    const refused = run("a.exe");
    assert.equal(refused.status, 1);
    assert.match(refused.stderr, /right-sign-windows: Windows signing must run on Windows/);
  }
  assert.equal(typeof main, "function");
});

const leftovers = () => fs.readdirSync(os.tmpdir()).filter((name) => name.startsWith("right-sign-")).sort();

test("a missing silent token is a typed failure before any signing: nothing is run, copied or left behind, and nothing exits", (t) => {
  const before = leftovers();
  const cases = [
    { token: { status: 1, stdout: "", stderr: "ERROR: Can't find token from MSAL cache. eyJhbGciOi.abc.def" }, match: /exit 1: ERROR: Can't find token from MSAL cache/ },
    { token: { status: 0, stdout: "eyJhbGciOiSecretTokenBody.payload.sig\n", stderr: "" }, match: /something other than an expiry/ },
    { token: { status: 0, stdout: "2000-01-01 00:00:00.000000\n", stderr: "" }, match: /under 10 minutes/ },
    { token: { status: null, stdout: "", stderr: "", error: new Error("spawn cmd.exe ENOENT") }, match: /could not be run/ },
  ];
  for (const { token, match } of cases) {
    const s = setup(t, { token });
    const receipt = path.join(s.dir, "r", "receipt.json");
    const bytes = fs.readFileSync(s.exe, "utf8");
    let caught;
    try { signWindowsFiles([s.exe], { ctx: s.ctx, receiptPath: receipt }); } catch (error) { caught = error; }
    assert.ok(caught instanceof WindowsSigningTokenError && caught instanceof WindowsSigningError, String(caught));
    assert.equal(caught.code, TOKEN_UNAVAILABLE_CODE);
    assert.equal(caught.code, "SIGN_WINDOWS_TOKEN_UNAVAILABLE");
    assert.match(caught.message, match);
    assert.match(caught.message, /sign in to the Windows desktop session as the signing user/);
    assert.doesNotMatch(caught.message, /SecretTokenBody|eyJhbGciOi\./, "no token text is ever reported");
    assert.equal(s.tokenCalls.length, 1);
    assert.match(s.tokenCalls[0], /--resource https:\/\/codesigning\.azure\.net --query expiresOn -o tsv/);
    assert.deepEqual(s.calls, [], "signtool never ran");
    assert.equal(fs.readFileSync(s.exe, "utf8"), bytes, "the file is untouched");
    assert.equal(fs.existsSync(receipt), false, "no receipt");
  }
  assert.deepEqual(leftovers(), before, "no temp directory left behind");
});

test("the CLI turns the typed token failure into exit 1 with its code, after cleanup", (t) => {
  const s = setup(t, { token: { status: 1, stdout: "", stderr: "Can't find token from MSAL cache" } });
  const err = [];
  const original = console.error;
  console.error = (line) => err.push(line);
  let code;
  try { code = main([s.exe], { run: (cmd, args) => (args[0] === "/d" ? { status: 1, stdout: "", stderr: "Can't find token from MSAL cache" } : { status: 1, stdout: "", stderr: "?" }), exists: () => true, platform: "win32", userEnvReader: () => "", quiet: true, processEnv: { PATH: "C:\\x" } }); } finally { console.error = original; }
  assert.equal(code, 1);
  assert.match(err.join("\n"), /SIGN_WINDOWS_TOKEN_UNAVAILABLE/);
});

test("no signing path calls process.exit but the CLI entry", () => {
  for (const file of ["sign-windows.mjs", "suite-signing.mjs", "suite-signing-policy.mjs", "install-tauri-authenticode.mjs"]) {
    const exits = fs.readFileSync(new URL(`./${file}`, import.meta.url), "utf8").split("\n").filter((line) => /process\.exit\(/.test(line) && !line.trimStart().startsWith("//"));
    if (file === "sign-windows.mjs") assert.equal(exits.length, 1, "only the entry line exits");
    else assert.deepEqual(exits, [], file);
  }
  const entry = fs.readFileSync(new URL("./sign-windows.mjs", import.meta.url), "utf8").split("\n").find((line) => /process\.exit\(/.test(line));
  assert.match(entry, /import\.meta\.url === pathToFileURL.*process\.exit\(main\(\)\)/, "the exit is main()'s return value, after its cleanup");
});
