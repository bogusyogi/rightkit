// rightkit#1303: the `sign` step of tests.suites. Fakes only: no codesign, no signtool, no Keychain, no signing agent, no broker. The fakes model what
// the real signers report (codesign -dvv, -d -r-, --verify; signtool verify) closely enough that a signer which signs the wrong thing, or reports the wrong
// identity, is caught by the readback this step does, never by the fake agreeing with itself.
import assert from "node:assert/strict";
import { EventEmitter } from "node:events";
import { createHash } from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { appendLedger, readLedger } from "./cargo-ledger.mjs";
import { SHIM_MAP_SCHEMA } from "./cargo-target-shim.mjs";
import { BUILD_LAUNCH_REFUSED, SIGN_STEP_COMMAND, buildLaunch } from "./build-launch.mjs";
import { MacosDevelopmentSigningError, expectedDesignatedRequirement } from "./development-signing.mjs";
import { runSignStep } from "./suite-signing.mjs";
import { QA_APP_BINARY_ENV, QA_SIGNED_FILES_ENV, QA_SIGNING_EVIDENCE_ENV, SIGN_SUBJECT, SIGN_TEAM, SuiteSigningError, containedRelative, validateSignSpec } from "./suite-signing-policy.mjs";
import { planTests, runSuites } from "./test-command.mjs";

const TEAM = SIGN_TEAM;
const BUNDLE_ID = "app.rightsuite.rightapp";
const IDENTITY = `Developer ID Application: Damned Ventures LLC (${TEAM})`;
const sha = (file) => createHash("sha256").update(fs.readFileSync(file)).digest("hex");
const tmp = (t, prefix = "rk-sign-") => { const d = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), prefix))); t.after(() => fs.rmSync(d, { recursive: true, force: true })); return d; };

/** An app root, a carrier map (snapshot, checkout, ledger), one receipt output directory holding the unsigned binary, and a signing directory. */
function fixture(t, { files = { "aarch64-apple-darwin/debug/rightapp": "UNSIGNED-MAIN" }, bundleId = BUNDLE_ID, mainBinaryName } = {}) {
  const dir = tmp(t);
  const appRoot = path.join(dir, "snapshot", "app");
  const checkout = path.join(dir, "checkout");
  const carrier = path.join(dir, "carrier");
  const outputDir = path.join(dir, "broker-output", "gen-1");
  const signingDirectory = path.join(dir, "managed", "suite-signing");
  for (const d of [path.join(appRoot, "src-tauri"), checkout, path.join(carrier, "ledger"), outputDir]) fs.mkdirSync(d, { recursive: true });
  fs.writeFileSync(path.join(appRoot, "src-tauri", "tauri.conf.json"), JSON.stringify({ productName: "RightApp", identifier: bundleId, ...(mainBinaryName ? { mainBinaryName } : {}) }));
  for (const [rel, body] of Object.entries(files)) { fs.mkdirSync(path.dirname(path.join(outputDir, rel)), { recursive: true }); fs.writeFileSync(path.join(outputDir, rel), body, { mode: 0o755 }); }
  const mapFile = path.join(carrier, "cargo-target-map.json");
  fs.writeFileSync(mapFile, JSON.stringify({ schema: SHIM_MAP_SCHEMA, mode: "broker", identity: "x", ledgerDir: path.join(carrier, "ledger"), workspaces: {}, broker: { snapshot: { path: path.join(dir, "snapshot") }, checkout, sentinel: path.join(carrier, "unbound") } }));
  const env = { RIGHTKIT_CARRIED_CARGO_MAP: mapFile };
  return { dir, appRoot, checkout, carrier, outputDir, signingDirectory, mapFile, env, ledgerDir: path.join(carrier, "ledger"), snapshot: path.join(dir, "snapshot") };
}

/**
 * A fake Developer ID signer and `codesign` reporter. `signOne` "signs" the copy it is handed (appends bytes), recording the identifier it was told;
 * `run` answers codesign -dvv, --verify and -d -r- from that record, so every readback is as real codesign would report it for what was signed.
 */
function fakeMac({ team = TEAM, identity = IDENTITY, adhoc = false, reportTeam, reportIdentifier, requirementOf, resolveError, signStatus = 0 } = {}) {
  const signed = new Map();
  const calls = [];
  return {
    signed, calls,
    resolveSigning: () => {
      if (resolveError) throw resolveError;
      return { identity, certificateSha1: "A".repeat(40), teamIdentifier: team, entitlements: null, provisioningProfile: null, ...(adhoc ? { adhoc: true } : {}) };
    },
    signOne: (args) => {
      const file = args.at(-1);
      const at = args.indexOf("--identifier");
      calls.push({ file, identifier: at >= 0 ? args[at + 1] : undefined, identity: args[args.indexOf("--sign") + 1], args, content: fs.readFileSync(file, "utf8") });
      if (signStatus !== 0) return { status: signStatus, stdout: "", stderr: "errSecInternalComponent" };
      fs.appendFileSync(file, "|SIGNED");
      signed.set(file, { identifier: at >= 0 ? args[at + 1] : path.basename(file) });
      return { status: 0, stdout: "", stderr: "" };
    },
    run: (command, args) => {
      if (command !== "codesign") return { status: 1, stdout: "", stderr: "unexpected command" };
      const file = args.at(-1);
      const record = signed.get(file);
      if (!record) return { status: 1, stdout: "", stderr: `${file}: code object is not signed at all` };
      const identifier = reportIdentifier ?? record.identifier;
      if (args.includes("-r-")) {
        const text = requirementOf ? requirementOf(identifier) : expectedDesignatedRequirement(identifier, reportTeam ?? team);
        return { status: 0, stdout: `Executable=${file}\ndesignated => ${text}\n`, stderr: "" };
      }
      if (args.includes("--verify")) return { status: 0, stdout: "", stderr: `${file}: valid on disk\n${file}: satisfies its Designated Requirement\n` };
      if (args.includes("-dvv")) return { status: 0, stdout: "", stderr: `Executable=${file}\nIdentifier=${identifier}\nFormat=Mach-O thin (arm64)\nAuthority=${identity}\nAuthority=Developer ID Certification Authority\nTimestamp=Oct 30, 2026 at 10:00:00\nTeamIdentifier=${reportTeam ?? team}\n` };
      return { status: 1, stdout: "", stderr: "unexpected codesign call" };
    },
  };
}

const MAC_FILES = [{ from: "build-app", path: "aarch64-apple-darwin/debug/rightapp" }];
const stepOf = (f, over = {}) => ({ name: "sign-app", sign: { files: MAC_FILES }, appRoot: f.appRoot, config: {}, env: f.env, platform: "darwin", outputDirsOf: (suite) => (suite === "build-app" ? [f.outputDir] : null), ...over });
const run = (f, mac, over = {}) => runSignStep(stepOf(f, { deps: { signingDirectory: f.signingDirectory, resolveSigning: mac.resolveSigning, signOne: mac.signOne, run: mac.run }, ...over }));
const code = (fn) => { try { fn(); } catch (error) { assert.ok(error instanceof SuiteSigningError, `${error?.constructor?.name}: ${error?.message}`); return error.code; } assert.fail("expected a refusal"); };

// ---- macOS: the identity, derived and read back ----------------------------------------------------------------------------------

test("mac: a main binary is signed as a COPY with the bundle id and the team, read back, and recorded; the source bytes never change", (t) => {
  const f = fixture(t);
  const mac = fakeMac();
  const source = path.join(f.outputDir, "aarch64-apple-darwin/debug/rightapp");
  const before = sha(source);
  const result = run(f, mac);
  assert.equal(sha(source), before, "the receipt's own file is untouched");
  assert.notEqual(result.appBinary, source);
  assert.ok(fs.readFileSync(result.appBinary, "utf8").endsWith("|SIGNED"));
  for (const place of [f.snapshot, f.checkout, f.outputDir]) assert.ok(path.relative(place, result.appBinary).split(path.sep).includes(".."), `the signed copy is outside ${place}`);
  assert.equal(mac.calls.length, 1);
  assert.equal(mac.calls[0].identifier, BUNDLE_ID, "the main binary is signed with the bundle id");
  assert.equal(mac.calls[0].identity, IDENTITY);
  const [evidence] = result.evidence;
  assert.deepEqual({ ...evidence, signed: undefined, sha256Before: undefined, sha256After: undefined }, { platform: "mac", file: "aarch64-apple-darwin/debug/rightapp", signed: undefined, sha256Before: undefined, sha256After: undefined, identifier: BUNDLE_ID, team: TEAM, identity: IDENTITY, requirement: expectedDesignatedRequirement(BUNDLE_ID, TEAM) });
  assert.equal(evidence.sha256Before, before);
  assert.equal(evidence.sha256After, sha(result.appBinary));
  assert.notEqual(evidence.sha256Before, evidence.sha256After);
  const written = JSON.parse(fs.readFileSync(result.evidenceFile, "utf8"));
  assert.deepEqual(written.files, result.evidence);
  assert.equal(written.team, TEAM);
  // The stage record: one `ran` entry in the carrier's ledger carrying the evidence.
  const entry = readLedger(f.ledgerDir, undefined).entries.find((e) => e.mode === "signing");
  assert.equal(entry.status, "ran");
  assert.deepEqual(entry.signing, result.evidence);
});

test("mac: the designated requirement is the release-shaped one, with the team quoted because it starts with a digit", () => {
  assert.equal(expectedDesignatedRequirement("a.b", "6KLGD3LLKF"), 'identifier "a.b" and anchor apple generic and certificate 1[field.1.2.840.113635.100.6.2.6] /* exists */ and certificate leaf[field.1.2.840.113635.100.6.1.13] /* exists */ and certificate leaf[subject.OU] = "6KLGD3LLKF"');
  assert.match(expectedDesignatedRequirement("a.b", "H7L2FQ3UA5"), /leaf\[subject\.OU\] = H7L2FQ3UA5$/);
  assert.throws(() => expectedDesignatedRequirement("a b", TEAM), MacosDevelopmentSigningError);
  assert.throws(() => expectedDesignatedRequirement("a.b", "short"), MacosDevelopmentSigningError);
});

test("mac: ad hoc is refused every way it can be asked for or arrive", (t) => {
  const f = fixture(t);
  assert.equal(code(() => run(f, fakeMac({ adhoc: true }))), "SIGN_IDENTITY_REFUSED");
  assert.equal(code(() => run(f, fakeMac(), { env: { ...f.env, RIGHTKIT_DEV_SIGNING: "ci-adhoc" } })), "SIGN_ADHOC_REFUSED");
  assert.equal(code(() => run(f, fakeMac(), { config: { development: { targets: { mac: { install: { signing: "ci-adhoc" } } } } } })), "SIGN_ADHOC_REFUSED");
  // A signer that reports an ad hoc signature on the copy is caught by the readback, whatever it resolved.
  const mac = fakeMac();
  const adhocRun = (command, args) => (args.includes("-dvv") ? { status: 0, stdout: "", stderr: `Identifier=${BUNDLE_ID}\nAuthority=${IDENTITY}\nTimestamp=x\nSignature=adhoc\nTeamIdentifier=not set\n` } : mac.run(command, args));
  assert.match(code(() => run(f, mac, { deps: { signingDirectory: f.signingDirectory, resolveSigning: mac.resolveSigning, signOne: mac.signOne, run: adhocRun } })), /^SIGN_(ADHOC|TEAM_MISMATCH)$/);
});

test("mac: Apple Development, another team and a missing credential are refused, with no fallback", (t) => {
  const f = fixture(t);
  assert.equal(code(() => run(f, fakeMac({ identity: `Apple Development: Someone (${TEAM})` }))), "SIGN_IDENTITY_REFUSED");
  assert.equal(code(() => run(f, fakeMac({ identity: `Mac Developer: Someone (${TEAM})` }))), "SIGN_IDENTITY_REFUSED");
  assert.equal(code(() => run(f, fakeMac({ team: "ABCDE12345", identity: "Developer ID Application: Other Org (ABCDE12345)" }))), "SIGN_TEAM_MISMATCH");
  assert.equal(code(() => run(f, fakeMac({ team: "ABCDE12345" }))), "SIGN_TEAM_MISMATCH", "an identity that names our team but resolves to another is refused too");
  const missing = new MacosDevelopmentSigningError("DEV_MAC_SIGNING_DEVELOPER_ID_REQUIRED", "no Developer ID Application identity with its private key is in Keychain");
  const error = (() => { try { run(f, fakeMac({ resolveError: missing })); } catch (e) { return e; } return null; })();
  assert.equal(error.code, "SIGN_CREDENTIAL_MISSING");
  assert.equal(error.reason, "DEV_MAC_SIGNING_DEVELOPER_ID_REQUIRED");
  assert.deepEqual(fs.readdirSync(f.outputDir), ["aarch64-apple-darwin"], "nothing was written beside the receipt");
  // The signer itself failing (key unavailable) is a refusal, and nothing is reported signed.
  assert.equal(code(() => run(f, fakeMac({ signStatus: 1 }))), "SIGN_FAILED");
});

test("mac: the read-back identifier, team and requirement must be the release ones: a mismatch of any of the three is refused", (t) => {
  const f = fixture(t);
  // The signer ignored --identifier: the release signing loop (signMacosEntries) catches it, and the step reports it as a failed sign naming the mismatch.
  const ignored = (() => { try { run(f, fakeMac({ reportIdentifier: "app.other" })); } catch (e) { return e; } return null; })();
  assert.equal(ignored.code, "SIGN_FAILED");
  assert.match(ignored.message, /identifier mismatch/);
  assert.match(code(() => run(f, fakeMac({ reportTeam: "ABCDE12345" }))), /^SIGN_(TEAM_MISMATCH|REQUIREMENT_MISMATCH|FAILED)$/);
  // A cdhash-bound requirement changes on every build, so macOS would drop every grant: refused although assertStableDesignatedRequirement stays silent about it.
  assert.equal(code(() => run(f, fakeMac({ requirementOf: () => 'cdhash H"0123456789abcdef0123456789abcdef01234567"' }))), "SIGN_REQUIREMENT_MISMATCH");
  // The short form, a different identifier inside the requirement, and an Apple Development shaped requirement are not the release's.
  assert.equal(code(() => run(f, fakeMac({ requirementOf: (id) => `identifier "${id}" and anchor apple generic and certificate leaf[subject.OU] = "${TEAM}"` }))), "SIGN_REQUIREMENT_MISMATCH");
  assert.equal(code(() => run(f, fakeMac({ requirementOf: () => expectedDesignatedRequirement("app.other", TEAM) }))), "SIGN_REQUIREMENT_MISMATCH");
  assert.equal(code(() => run(f, fakeMac({ requirementOf: (id) => `identifier "${id}" and anchor apple generic and certificate leaf[field.1.2.840.113635.100.6.1.12] /* exists */ and certificate leaf[subject.OU] = "${TEAM}"` }))), "SIGN_REQUIREMENT_MISMATCH");
  // The requirement cannot be read at all: refused, not skipped.
  const mac = fakeMac();
  const noRequirement = (command, args) => (args.includes("-r-") ? { status: 1, stdout: "", stderr: "code object is not signed at all" } : mac.run(command, args));
  assert.equal(code(() => run(f, mac, { deps: { signingDirectory: f.signingDirectory, resolveSigning: mac.resolveSigning, signOne: mac.signOne, run: noRequirement } })), "SIGN_REQUIREMENT_MISMATCH");
});

test("mac: the identifier is the tauri.conf.json one; install.bundleId must equal it and a platform file may not change it", (t) => {
  const f = fixture(t);
  assert.equal(code(() => run(f, fakeMac(), { config: { development: { targets: { mac: { install: { bundleId: "app.other" } } } } } })), "SIGN_IDENTIFIER_MISMATCH");
  const agree = run(f, fakeMac(), { config: { development: { targets: { mac: { install: { bundleId: BUNDLE_ID } } } } } });
  assert.equal(agree.evidence[0].identifier, BUNDLE_ID);
  fs.writeFileSync(path.join(f.appRoot, "src-tauri", "tauri.macos.conf.json"), JSON.stringify({ identifier: "app.rightsuite.rightapp.dev" }));
  assert.equal(code(() => run(f, fakeMac())), "SIGN_IDENTIFIER_OVERRIDE");
  fs.rmSync(path.join(f.appRoot, "src-tauri", "tauri.macos.conf.json"));
  fs.rmSync(path.join(f.appRoot, "src-tauri", "tauri.conf.json"));
  assert.equal(code(() => run(f, fakeMac())), "SIGN_IDENTIFIER_UNREADABLE");
});

test("mac: sidecars are signed with their release identifier; one the release signs with none declared gets the name codesign derives, as in the release", (t) => {
  const f = fixture(t, { files: { "debug/rightapp": "MAIN", "debug/engine": "ENGINE" } });
  const files = [{ from: "build-app", path: "debug/rightapp" }, { from: "build-app", path: "debug/engine" }];
  const derived = fakeMac();
  const undeclared = run(f, derived, { sign: { files } });
  assert.deepEqual(derived.calls.map((c) => c.identifier), [BUNDLE_ID, undefined], "no --identifier is passed for the sidecar, exactly as the release signs it");
  assert.equal(undeclared.evidence[1].identifier, "engine");
  assert.equal(undeclared.evidence[1].requirement, expectedDesignatedRequirement("engine", TEAM));
  // The siblings stay siblings: the main binary and its sidecar are copied under one directory.
  assert.equal(path.dirname(derived.calls[0].file), path.dirname(derived.calls[1].file));
  assert.equal(code(() => run(fixture(t, { files: { "a/x": "1", "b/x": "2" } }), fakeMac(), { sign: { files: [{ from: "build-app", path: "a/x" }, { from: "build-app", path: "a/x" }] } })), "SIGN_COPY_COLLISION", "two copies may not land on one path");
  const mac = fakeMac();
  const config = { development: { targets: { mac: { install: { identifiers: { "Contents/MacOS/engine": `${BUNDLE_ID}.engine` } } } } } };
  const result = run(f, mac, { sign: { files }, config });
  assert.deepEqual(mac.calls.map((c) => c.identifier), [BUNDLE_ID, `${BUNDLE_ID}.engine`], "the main binary first, the sidecar under its release identifier");
  assert.equal(result.evidence[1].requirement, expectedDesignatedRequirement(`${BUNDLE_ID}.engine`, TEAM));
  assert.equal(path.basename(result.appBinary), "rightapp", "RIGHTKIT_QA_APP_BINARY is the first file's signed copy");
  const template = run(fixture(t, { files: { "debug/rightapp": "MAIN", "debug/engine": "ENGINE" } }), fakeMac(), { sign: { files }, config: { targets: { mac: { sign: { identifierTemplate: "<bundleId>.<basename>" } } } } });
  assert.equal(template.evidence[1].identifier, `${BUNDLE_ID}.engine`);
});

test("mac: the main binary is the one tauri.conf.json's mainBinaryName names, wherever it is listed; a step that signs only sidecars sets no app binary", (t) => {
  const files = { "debug/engine": "ENGINE", "debug/rightapp": "MAIN" };
  const engineFirst = [{ from: "build-app", path: "debug/engine" }, { from: "build-app", path: "debug/rightapp" }];
  const config = { development: { targets: { mac: { install: { identifiers: { "Contents/MacOS/engine": `${BUNDLE_ID}.engine` } } } } } };
  const f = fixture(t, { files, mainBinaryName: "rightapp" });
  const mac = fakeMac();
  const result = run(f, mac, { sign: { files: engineFirst }, config });
  assert.deepEqual(mac.calls.map((c) => [path.basename(c.file), c.identifier]), [["engine", `${BUNDLE_ID}.engine`], ["rightapp", BUNDLE_ID]], "the sidecar keeps its release identifier and the named main binary gets the bundle id, whatever the order");
  assert.equal(path.basename(result.appBinary), "rightapp");
  // Only the sidecar: no main binary was signed, so there is no RIGHTKIT_QA_APP_BINARY to hand on.
  const g = fixture(t, { files, mainBinaryName: "rightapp" });
  const only = run(g, fakeMac(), { sign: { files: [engineFirst[0]] }, config });
  assert.equal(only.appBinary, null);
  assert.equal(only.evidence[0].identifier, `${BUNDLE_ID}.engine`);
  // Two files with the main binary's name are ambiguous.
  const h = fixture(t, { files: { "a/rightapp": "X", "b/rightapp": "Y" }, mainBinaryName: "rightapp" });
  assert.equal(code(() => run(h, fakeMac(), { sign: { files: [{ from: "build-app", path: "a/rightapp" }, { from: "build-app", path: "b/rightapp" }] } })), "SIGN_MAIN_AMBIGUOUS");
});

// ---- where the file comes from ----------------------------------------------------------------------------------------------------

test("sources: `from` must be a suite that ran; a path may not leave the receipt's output directory; a link is not the receipt's file", (t) => {
  const f = fixture(t);
  assert.equal(code(() => run(f, fakeMac(), { outputDirsOf: () => null })), "SIGN_FROM_NOT_RUN");
  assert.equal(code(() => run(f, fakeMac(), { sign: { files: [{ from: "build-app", path: "../outside" }] } })), "SIGN_STEP_PATH");
  assert.equal(code(() => run(f, fakeMac(), { sign: { files: [{ from: "build-app", path: "/etc/passwd" }] } })), "SIGN_STEP_PATH");
  assert.equal(code(() => run(f, fakeMac(), { sign: { files: [{ from: "build-app", path: "aarch64-apple-darwin/missing" }] } })), "SIGN_SOURCE_MISSING");
  fs.writeFileSync(path.join(f.dir, "secret"), "OUTSIDE");
  fs.symlinkSync(path.join(f.dir, "secret"), path.join(f.outputDir, "link"));
  assert.equal(code(() => run(f, fakeMac(), { sign: { files: [{ from: "build-app", path: "link" }] } })), "SIGN_PATH_ESCAPE");
  fs.symlinkSync(f.dir, path.join(f.outputDir, "linkdir"));
  assert.equal(code(() => run(f, fakeMac(), { sign: { files: [{ from: "build-app", path: "linkdir/secret" }] } })), "SIGN_PATH_ESCAPE");
  assert.equal(fs.readFileSync(path.join(f.dir, "secret"), "utf8"), "OUTSIDE");
  // No carried receipts at all: nothing to name a file by.
  assert.equal(code(() => run(f, fakeMac(), { env: {} })), "SIGN_RECEIPT_MISSING");
});

test("sources: the signed copy never lives in the snapshot, the checkout or a broker output directory", (t) => {
  const f = fixture(t);
  for (const inside of [path.join(f.snapshot, "signed"), path.join(f.checkout, "signed"), path.join(f.outputDir, "signed")]) {
    assert.equal(code(() => runSignStep(stepOf(f, { deps: { signingDirectory: inside, resolveSigning: fakeMac().resolveSigning, signOne: fakeMac().signOne, run: fakeMac().run } }))), "SIGN_COPY_PLACE", inside);
  }
  assert.equal(fs.existsSync(path.join(f.snapshot, "signed")), false);
});

test("a failed sign leaves a refused entry in the ledger, so the stage fails with it", (t) => {
  const f = fixture(t);
  assert.equal(code(() => run(f, fakeMac({ adhoc: true }))), "SIGN_IDENTITY_REFUSED");
  const entry = readLedger(f.ledgerDir, undefined).entries.find((e) => e.mode === "signing");
  assert.equal(entry.status, "refused");
  assert.equal(entry.code, "SIGN_IDENTITY_REFUSED");
});

// ---- Windows ----------------------------------------------------------------------------------------------------------------------

const WIN_FILES = [{ from: "build-app", path: "x86_64-pc-windows-msvc/debug/rightapp.exe" }];
const winEvidence = (file, over = {}) => ({ file, before: { sha256: sha(file), sizeBytes: 1 }, after: { sha256: "b".repeat(64), sizeBytes: 2 }, authenticode: "Valid", subject: SIGN_SUBJECT, timestampPresent: true, ...over });

test("win: files are signed through sign-windows as copies, checked against the signer SUBJECT, never a thumbprint", (t) => {
  const f = fixture(t, { files: { "x86_64-pc-windows-msvc/debug/rightapp.exe": "MZ-UNSIGNED" } });
  const source = path.join(f.outputDir, "x86_64-pc-windows-msvc/debug/rightapp.exe");
  const before = sha(source);
  const signed = [];
  const deps = { signingDirectory: f.signingDirectory, signerPreflight: () => {}, signWindows: (files) => { signed.push(...files); return files.map((file) => winEvidence(file)); }, verifyWindows: (files) => files.map((file) => winEvidence(file)) };
  const result = runSignStep(stepOf(f, { platform: "win32", sign: { files: WIN_FILES }, deps }));
  assert.equal(sha(source), before);
  assert.notEqual(signed[0], source, "the copy is signed, never the receipt's file");
  assert.equal(result.appBinary, signed[0]);
  assert.equal(result.evidence[0].subject, "CN=Damned Ventures LLC");
  assert.equal(JSON.parse(fs.readFileSync(result.evidenceFile, "utf8")).subject, "CN=Damned Ventures LLC");
  for (const subject of ["CN=Someone Else", "CN=Damned Ventures LLC Fake", undefined]) {
    const wrong = { ...deps, verifyWindows: (files) => files.map((file) => winEvidence(file, { subject })) };
    assert.equal(code(() => runSignStep(stepOf(f, { platform: "win32", sign: { files: WIN_FILES }, deps: wrong }))), "SIGN_SUBJECT_MISMATCH", String(subject));
  }
  const invalid = { ...deps, signWindows: (files) => files.map((file) => winEvidence(file, { authenticode: "Invalid" })) };
  assert.equal(code(() => runSignStep(stepOf(f, { platform: "win32", sign: { files: WIN_FILES }, deps: invalid }))), "SIGN_SUBJECT_MISMATCH");
  const broken = { ...deps, signWindows: () => { throw new Error("signtool.exe not found"); } };
  assert.equal(code(() => runSignStep(stepOf(f, { platform: "win32", sign: { files: WIN_FILES }, deps: broken }))), "SIGN_FAILED");
  assert.equal(code(() => runSignStep(stepOf(f, { platform: "linux" }))), "SIGN_PLATFORM");
});

// ---- the step in the suite list ---------------------------------------------------------------------------------------------------

const suites = (...entries) => ({ tests: { suites: entries } });
const build = { name: "build-app", cmd: "pnpm", args: ["exec", "tauri", "build", "--no-bundle"] };

test("plan: a sign step names an EARLIER suite, relative paths only, and nothing else", () => {
  const appRoot = path.resolve("fixture-app");
  const plan = planTests(suites(build, { name: "sign-app", sign: { files: MAC_FILES } }, { name: "qa", cmd: "node", args: ["x.mjs"] }), { appRoot });
  assert.deepEqual(plan.map((p) => p.name), ["build-app", "sign-app", "qa"]);
  assert.equal(plan[1].step, null);
  assert.deepEqual(plan[1].sign.files, MAC_FILES);
  const refused = (entries, expected) => assert.throws(() => planTests(suites(...entries), { appRoot }), (error) => error.code === expected, expected);
  refused([{ name: "sign-app", sign: { files: MAC_FILES } }, build], "SIGN_STEP_FROM");
  refused([build, { name: "sign-app", sign: { files: [{ from: "nope", path: "a" }] } }], "SIGN_STEP_FROM");
  refused([build, { name: "sign-app", sign: { files: [{ from: "sign-app", path: "a" }] } }], "SIGN_STEP_FROM");
  refused([build, { name: "sign-app", sign: { files: [{ from: "build-app", path: "../a" }] } }], "SIGN_STEP_PATH");
  refused([build, { name: "sign-app", sign: { files: [{ from: "build-app", path: "/a" }] } }], "SIGN_STEP_PATH");
  refused([build, { name: "sign-app", sign: { files: [{ from: "build-app", path: "C:\\a" }] } }], "SIGN_STEP_PATH");
  refused([build, { name: "sign-app", sign: { files: [{ from: "build-app", path: "a//b" }] } }], "SIGN_STEP_PATH");
  refused([build, { name: "sign-app", sign: { files: [] } }], "SIGN_STEP_SHAPE");
  refused([build, { name: "sign-app", sign: { files: MAC_FILES, identity: "-" } }], "SIGN_STEP_SHAPE");
  refused([build, { name: "sign-app", sign: { files: [{ from: "build-app", path: "a", identifier: "x.y" }] } }], "SIGN_STEP_SHAPE");
  refused([build, { name: "sign-app", cmd: "x", sign: { files: MAC_FILES } }], "SIGN_STEP_SHAPE");
  refused([build, { name: "sign-app", env: { A: "1" }, sign: { files: MAC_FILES } }], "SIGN_STEP_SHAPE");
  refused([build, { name: "build-app", sign: { files: MAC_FILES } }], "RK_COMMAND_SPEC");
  assert.throws(() => planTests(suites(build, { name: "sign-app", sign: { files: MAC_FILES } }), { appRoot, suites: ["sign-app"], passthrough: ["x"] }), /cannot go to a sign step/);
  assert.equal(containedRelative("a/b\\c", "p"), "a/b/c");
  assert.throws(() => validateSignSpec("x", "l", new Set()), SuiteSigningError);
});

/** A spawn that plays a suite: runs `play` (which may write ledger entries) and exits with the next code. */
function player(plays, spawned) {
  let i = 0;
  return (command, args, options) => {
    const play = plays[i]; i += 1;
    spawned.push({ command, args, env: options.env });
    const child = new EventEmitter();
    setImmediate(() => { play?.(); child.emit("exit", 0, null); });
    return child;
  };
}

test("runSuites: the sign step runs in-process, reads the build suite's own receipts, sets RIGHTKIT_QA_APP_BINARY only after success, and cleans its copies up", async (t) => {
  const f = fixture(t);
  const appRoot = f.appRoot;
  const plan = planTests(suites(build, { name: "sign-app", sign: { files: MAC_FILES } }, { name: "qa", cmd: "node", args: ["qa.mjs"] }), { appRoot });
  const spawned = []; const doors = [];
  const mac = fakeMac();
  const logs = [];
  const spawnFn = player([() => appendLedger(f.ledgerDir, undefined, { id: "build-1", mode: "broker", status: "verified", adapter: "rightkit-pnpm", argv: ["exec", "tauri", "build"], cwd: appRoot, outputDir: f.outputDir }), null], spawned);
  const result = await runSuites(plan, { appRoot, env: { ...f.env, KEEP: "1" }, platform: "darwin", config: {}, spawnFn, door: (command, args) => { doors.push([command, ...args]); return { command, args }; }, log: (line) => logs.push(JSON.parse(line)), errLog: () => {}, signDeps: { signingDirectory: f.signingDirectory, resolveSigning: mac.resolveSigning, signOne: mac.signOne, run: mac.run } });
  assert.equal(result.exitCode, 0);
  assert.equal(doors.length, 2, "the door saw the two suites, never the sign step");
  assert.equal(spawned.length, 2, "nothing was spawned for the sign step");
  assert.equal(spawned[0].env[QA_APP_BINARY_ENV], undefined, "the suite before the sign step has no signed binary");
  const signedEvent = logs.find((e) => e.event === "test_suite_signed");
  assert.equal(spawned[1].env[QA_APP_BINARY_ENV], signedEvent.appBinary, "the suite after it runs the signed copy");
  assert.equal(spawned[1].env.KEEP, "1");
  assert.equal(signedEvent.signing[0].identifier, BUNDLE_ID);
  assert.equal(fs.existsSync(signedEvent.appBinary), false, "the signed copy is removed when the run ends");
  assert.deepEqual(logs.filter((e) => e.event === "test_suite_started").map((e) => e.suite), ["build-app", "sign-app", "qa"]);
});

test("runSuites: a failed sign step stops the run, sets nothing, and its refusal is printed with its code", async (t) => {
  const f = fixture(t);
  const plan = planTests(suites(build, { name: "sign-app", sign: { files: MAC_FILES } }, { name: "qa", cmd: "node", args: ["qa.mjs"] }), { appRoot: f.appRoot });
  const spawned = []; const errors = [];
  const mac = fakeMac({ adhoc: true });
  const result = await runSuites(plan, { appRoot: f.appRoot, env: f.env, platform: "darwin", config: {}, spawnFn: player([() => appendLedger(f.ledgerDir, undefined, { id: "b", mode: "broker", status: "verified", argv: [], cwd: f.appRoot, outputDir: f.outputDir })], spawned), door: (c, a) => ({ command: c, args: a }), log: () => {}, errLog: (line) => errors.push(line), signDeps: { signingDirectory: f.signingDirectory, resolveSigning: mac.resolveSigning, signOne: mac.signOne, run: mac.run } });
  assert.equal(result.exitCode, 1);
  assert.equal(spawned.length, 1, "the suite after a failed sign never starts");
  assert.match(errors.join("\n"), /\[SIGN_IDENTITY_REFUSED\]/);
  assert.ok(spawned.every((s) => s.env[QA_APP_BINARY_ENV] === undefined));
});

test("runSuites: a sign step that names a suite that did not run in THIS run is refused (--suite sign-app alone)", async (t) => {
  const f = fixture(t);
  const plan = planTests(suites(build, { name: "sign-app", sign: { files: MAC_FILES } }), { appRoot: f.appRoot, suites: ["sign-app"] });
  const errors = [];
  const result = await runSuites(plan, { appRoot: f.appRoot, env: f.env, platform: "darwin", config: {}, spawnFn: () => { throw new Error("no spawn"); }, door: () => { throw new Error("no door"); }, log: () => {}, errLog: (line) => errors.push(line), signDeps: { signingDirectory: f.signingDirectory } });
  assert.equal(result.exitCode, 1);
  assert.match(errors.join("\n"), /\[SIGN_FROM_NOT_RUN\]/);
});

test("runSuites: receipts are attributed per suite: a sign step sees its `from` suite's own output directories, never another suite's", async (t) => {
  const f = fixture(t);
  const other = path.join(f.dir, "broker-output", "gen-2");
  fs.mkdirSync(path.join(other, "aarch64-apple-darwin/debug"), { recursive: true });
  fs.writeFileSync(path.join(other, "aarch64-apple-darwin/debug/rightapp"), "OTHER-SUITE-BUILD");
  const plan = planTests(suites({ name: "helper", cmd: "node", args: ["h.mjs"] }, build, { name: "sign-app", sign: { files: MAC_FILES } }), { appRoot: f.appRoot });
  const mac = fakeMac();
  const spawned = [];
  const result = await runSuites(plan, { appRoot: f.appRoot, env: f.env, platform: "darwin", config: {}, spawnFn: player([() => appendLedger(f.ledgerDir, undefined, { id: "helper-1", mode: "broker", status: "verified", argv: [], cwd: f.appRoot, outputDir: other }), () => appendLedger(f.ledgerDir, undefined, { id: "build-1", mode: "broker", status: "verified", argv: [], cwd: f.appRoot, outputDir: f.outputDir })], spawned), door: (c, a) => ({ command: c, args: a }), log: () => {}, errLog: () => {}, signDeps: { signingDirectory: f.signingDirectory, resolveSigning: mac.resolveSigning, signOne: mac.signOne, run: mac.run } });
  assert.equal(result.exitCode, 0);
  const copy = mac.calls[0].file;
  assert.ok(copy.endsWith(path.join("aarch64-apple-darwin", "debug", "rightapp")), "the copy keeps its relative path");
  assert.equal(mac.calls[0].content, "UNSIGNED-MAIN", "the file signed came from the build suite's receipt, not the helper's output directory");
  assert.equal(fs.existsSync(copy), false, "and the copy is gone when the run ends");
  assert.equal(mac.signed.size, 1);
});

test("a sign step is never carried: the door refuses its command on every route and the step has no command to run", () => {
  for (const env of [{}, { RIGHTKIT_CARRIED_CARGO_MAP: "/nowhere" }, { RK_GATE_CHECKOUT: "/x" }]) {
    assert.throws(() => buildLaunch(SIGN_STEP_COMMAND, ["sign-app"], { env, platform: "darwin" }), (error) => error.code === BUILD_LAUNCH_REFUSED && /in-process/.test(error.message));
    assert.throws(() => buildLaunch("node", ["x.mjs"], { env, platform: "darwin", signStep: true }), (error) => error.code === BUILD_LAUNCH_REFUSED);
  }
  assert.throws(() => planTests(suites({ name: "sign-app", cmd: "right-release", sign: { files: MAC_FILES } }), { appRoot: path.resolve("a") }), (error) => error.code === "SIGN_STEP_SHAPE");
});

// ---- an end-to-end consumer: main binary + sidecar, signed from their own suites, found beside each other, matched to the evidence -----------

const ENGINE_FILES = { main: "aarch64-apple-darwin/debug/heardright_next_bin", engine: "aarch64-apple-darwin/debug/heardright-engine" };

/**
 * What a QA consumer does (heardright qa-e2e/src/lib.rs install_layout_beside and app_binary: the engine sidecar `heardright-engine<EXE_SUFFIX>` sits in
 * the directory of the app binary), reimplemented here, never imported: resolve the directory of RIGHTKIT_QA_APP_BINARY, find the sidecar sibling, and
 * return what the evidence says about each. Throws (a refusal) when a signed sidecar is not there or is not the bytes the evidence recorded.
 */
function consume(env, { sidecar = "heardright-engine", suffix = "" } = {}) {
  const main = env[QA_APP_BINARY_ENV];
  const evidencePath = env[QA_SIGNING_EVIDENCE_ENV];
  assert.ok(main && evidencePath, "the run exports both the signed binary and its evidence");
  const evidence = JSON.parse(fs.readFileSync(evidencePath, "utf8"));
  const record = (name) => evidence.files.find((file) => path.posix.basename(file.file).replace(/\.exe$/i, "") === name);
  const sidecarFile = path.join(path.dirname(main), `${sidecar}${suffix}`);
  const sidecarRecord = record(sidecar);
  if (!sidecarRecord) throw new Error(`no signed ${sidecar} in the signing evidence: refusing a stale-engine fallback`);
  if (!fs.existsSync(sidecarFile)) throw new Error(`${sidecarFile} is missing beside the signed app binary: refusing a stale-engine fallback`);
  return { main, sidecarFile, evidence, mainRecord: record(path.basename(main)), sidecarRecord, sidecarSha: sha(sidecarFile), mainSha: sha(main) };
}

/** A two-suite fixture: `build-app` made the main binary, `build-engine` the sidecar, each in its own receipt output directory. */
function twoSuites(t, { enginePath = ENGINE_FILES.engine } = {}) {
  const f = fixture(t, { files: { [ENGINE_FILES.main]: "MAIN-UNSIGNED" } });
  const engineDir = path.join(f.dir, "broker-output", "gen-2");
  fs.mkdirSync(path.dirname(path.join(engineDir, enginePath)), { recursive: true });
  fs.writeFileSync(path.join(engineDir, enginePath), "ENGINE-UNSIGNED", { mode: 0o755 });
  return { f, engineDir, outputDirsOf: (suite) => ({ "build-app": [f.outputDir], "build-engine": [engineDir] })[suite] ?? null };
}
const TWO = [{ from: "build-app", path: ENGINE_FILES.main }, { from: "build-engine", path: ENGINE_FILES.engine }];

test("consumer: main and sidecar signed from their own suites land under ONE signed root, resolve beside each other, and match the evidence by sha256", (t) => {
  const { f, outputDirsOf } = twoSuites(t);
  const mac = fakeMac();
  const result = run(f, mac, { sign: { files: TWO }, outputDirsOf });
  const env = { [QA_APP_BINARY_ENV]: result.appBinary, [QA_SIGNING_EVIDENCE_ENV]: result.evidenceFile };
  const seen = consume(env);
  assert.equal(path.dirname(seen.sidecarFile), path.dirname(result.appBinary), "the sidecar is found beside the main binary");
  assert.ok(seen.sidecarFile.startsWith(`${result.signedDir}${path.sep}`) && result.appBinary.startsWith(`${result.signedDir}${path.sep}`), "both live under one signed root");
  assert.equal(seen.sidecarSha, seen.sidecarRecord.sha256After, "the resolved sidecar is the signed bytes the evidence recorded");
  assert.equal(seen.mainSha, seen.mainRecord.sha256After, "so is the main binary");
  assert.notEqual(seen.sidecarRecord.sha256Before, seen.sidecarRecord.sha256After);
  assert.equal(seen.evidence.files.length, 2);
  // Receipt-relative paths are kept (not flattened) and are what the evidence names.
  assert.deepEqual(seen.evidence.files.map((file) => file.file), [ENGINE_FILES.main, ENGINE_FILES.engine]);
  assert.equal(seen.sidecarFile, path.join(result.signedDir, ...ENGINE_FILES.engine.split("/")));
  // The same relative path from two suites collides, whatever suite it came from: one path, one file.
  const clash = twoSuites(t, { enginePath: ENGINE_FILES.main });
  assert.equal(code(() => run(clash.f, fakeMac(), { sign: { files: [{ from: "build-app", path: ENGINE_FILES.main }, { from: "build-engine", path: ENGINE_FILES.main }] }, outputDirsOf: clash.outputDirsOf })), "SIGN_COPY_COLLISION");
});

test("consumer: a missing signed sidecar is detectable from the evidence, so a stale-engine fallback is refused", (t) => {
  const { f, outputDirsOf } = twoSuites(t);
  // 1. The step signed only the main binary: the evidence names no engine, even though an (unsigned) engine sits somewhere.
  const only = run(f, fakeMac(), { sign: { files: [TWO[0]] }, outputDirsOf });
  assert.throws(() => consume({ [QA_APP_BINARY_ENV]: only.appBinary, [QA_SIGNING_EVIDENCE_ENV]: only.evidenceFile }), /no signed heardright-engine in the signing evidence: refusing a stale-engine fallback/);
  // 2. A stale engine copied beside the signed shell by something else: it is there, but it is not the bytes the evidence recorded for any file.
  const full = only;
  fs.writeFileSync(path.join(path.dirname(full.appBinary), "heardright-engine"), "STALE-ENGINE");
  const staleSha = sha(path.join(path.dirname(full.appBinary), "heardright-engine"));
  assert.ok(!full.evidence.some((record) => record.sha256After === staleSha), "no evidence record carries the stale engine's digest");
  assert.equal(full.evidence.find((record) => record.file.endsWith("heardright-engine")), undefined);
  // 3. Signed, then the sidecar disappears: the evidence names it, the file is gone.
  const signed = run(f, fakeMac(), { sign: { files: TWO }, outputDirsOf });
  fs.rmSync(path.join(path.dirname(signed.appBinary), "heardright-engine"));
  assert.throws(() => consume({ [QA_APP_BINARY_ENV]: signed.appBinary, [QA_SIGNING_EVIDENCE_ENV]: signed.evidenceFile }), /is missing beside the signed app binary: refusing a stale-engine fallback/);
});

test("consumer: runSuites exports RIGHTKIT_QA_SIGNING_EVIDENCE beside RIGHTKIT_QA_APP_BINARY, keeps the evidence until the run ends, and a consumer suite matches it", async (t) => {
  const { f, outputDirsOf: _unused, engineDir } = twoSuites(t);
  const appRoot = f.appRoot;
  const plan = planTests(suites(build, { name: "build-engine", cmd: "pnpm", args: ["exec", "tauri", "build", "--no-bundle"] }, { name: "sign-app", sign: { files: TWO } }, { name: "qa", cmd: "node", args: ["qa.mjs"] }), { appRoot });
  const mac = fakeMac();
  const outputs = [f.outputDir, engineDir];
  let build_i = 0;
  const seen = [];
  const spawnFn = (command, args, options) => {
    const child = new EventEmitter();
    const index = build_i; build_i += 1;
    setImmediate(() => {
      if (index < 2) appendLedger(f.ledgerDir, undefined, { id: `build-${index}`, mode: "broker", status: "verified", adapter: "rightkit-pnpm", argv: ["exec", "tauri", "build"], cwd: appRoot, outputDir: outputs[index] });
      else {
        assert.ok(fs.existsSync(options.env[QA_SIGNING_EVIDENCE_ENV]), "the evidence file exists while the consumer suite runs");
        seen.push({ ...consume(options.env), evidenceFile: options.env[QA_SIGNING_EVIDENCE_ENV] });
      }
      child.emit("exit", 0, null);
    });
    return child;
  };
  const result = await runSuites(plan, { appRoot, env: { ...f.env }, platform: "darwin", config: {}, spawnFn, door: (command, args) => ({ command, args }), log: () => {}, errLog: () => {}, signDeps: { signingDirectory: f.signingDirectory, resolveSigning: mac.resolveSigning, signOne: mac.signOne, run: mac.run } });
  assert.equal(result.exitCode, 0);
  assert.equal(seen.length, 1);
  assert.equal(seen[0].sidecarSha, seen[0].sidecarRecord.sha256After);
  assert.equal(seen[0].mainSha, seen[0].mainRecord.sha256After);
  assert.equal(fs.existsSync(seen[0].evidenceFile), false, "the evidence is removed when the stage ends");
});

test("win: a Windows signer with no silent token is refused FIRST: no signing directory, no copy, the signer never called", (t) => {
  const f = fixture(t, { files: { "x86_64-pc-windows-msvc/debug/rightapp.exe": "MZ-UNSIGNED" } });
  const token = Object.assign(new Error("SIGN_WINDOWS_TOKEN_UNAVAILABLE: the Azure signer has no silent token"), { code: "SIGN_WINDOWS_TOKEN_UNAVAILABLE" });
  let preflights = 0; let signed = 0; let verified = 0;
  const copies = [];
  const realCopy = fs.copyFileSync;
  t.mock.method(fs, "copyFileSync", (...args) => { copies.push(args); return realCopy(...args); });
  const deps = { signingDirectory: f.signingDirectory, signerPreflight: () => { preflights += 1; throw token; }, signWindows: () => { signed += 1; return []; }, verifyWindows: () => { verified += 1; return []; } };
  assert.equal(code(() => runSignStep(stepOf(f, { platform: "win32", sign: { files: WIN_FILES }, deps }))), "SIGN_WINDOWS_TOKEN_UNAVAILABLE");
  assert.equal(preflights, 1);
  assert.equal(fs.existsSync(f.signingDirectory), false, "the signing directory was never created");
  assert.deepEqual(copies, [], "nothing was copied");
  assert.equal(signed + verified, 0, "the signer and verifier were never called");
  const ledger = readLedger(f.ledgerDir, undefined).entries.filter((e) => e.mode === "signing");
  assert.deepEqual(ledger.map((e) => [e.status, e.code]), [["refused", "SIGN_WINDOWS_TOKEN_UNAVAILABLE"]]);
  // A preflight that passes lets the step go on to the signer.
  const ok = { ...deps, signerPreflight: () => {}, signWindows: (files) => files.map((file) => winEvidence(file)), verifyWindows: (files) => files.map((file) => winEvidence(file)) };
  assert.equal(runSignStep(stepOf(f, { platform: "win32", sign: { files: WIN_FILES }, deps: ok })).evidence.length, 1);
});

test("[GB-42] every file a sign step signed (main binary and each declared sidecar) is mapped to its signed copy for the consumer; the raw target is never what the map names", (t) => {
  const f = fixture(t, { files: { "debug/rightapp": "MAIN", "debug/engine": "ENGINE", "debug/helper": "HELPER" }, mainBinaryName: "rightapp" });
  const files = [{ from: "build-app", path: "debug/engine" }, { from: "build-app", path: "debug/rightapp" }, { from: "build-app", path: "debug/helper" }];
  const result = run(f, fakeMac(), { sign: { files } });
  assert.deepEqual(Object.keys(result.signedFiles).sort(), ["debug/engine", "debug/helper", "debug/rightapp"], "all declared sidecars, not only mainBinaryName");
  for (const [declared, signedCopy] of Object.entries(result.signedFiles)) {
    assert.ok(fs.existsSync(signedCopy), declared);
    assert.ok(!signedCopy.startsWith(f.outputDir), `${declared}: the mapped bytes are the signed copy outside the build's output directory, never the raw target`);
    assert.match(fs.readFileSync(signedCopy, "utf8"), /\|SIGNED$/, `${declared} is the signed copy`);
  }
  assert.equal(result.signedFiles["debug/rightapp"], result.appBinary);
  assert.equal(QA_SIGNED_FILES_ENV, "RIGHTKIT_QA_SIGNED_FILES");
});
