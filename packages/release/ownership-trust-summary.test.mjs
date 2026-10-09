import assert from "node:assert/strict";
import { generateKeyPairSync, sign } from "node:crypto";
import { mkdtempSync, rmSync, writeFileSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { createHash } from "node:crypto";
import test from "node:test";
import { createKeyEvent, keyFingerprint } from "./ownership-key-log.mjs";
import { prepareTrustUpdate } from "./ownership-key-trust.mjs";
import { computePolicyBundle } from "./ownership-policy-bundle.mjs";
import { summarizeRequestFile, summarizeTrustChange } from "./ownership-trust-summary.mjs";
import { appleScriptText, buildMacConfirmScript, buildWindowsConfirmScript, capSummary, confirmTrustChange } from "./ownership-key-elevation.mjs";
import { __testOnly } from "./ownership-key-command.mjs";

function makeKey(platform, deviceName) {
  const pair = generateKeyPairSync("ec", { namedCurve: "prime256v1" });
  const publicKeyPem = pair.publicKey.export({ type: "spki", format: "pem" });
  return { privateKey: pair.privateKey, record: { keyId: keyFingerprint(publicKeyPem), publicKeyPem, platform, deviceName } };
}
function canonical(v) {
  if (v === null || typeof v !== "object") return JSON.stringify(v);
  if (Array.isArray(v)) return `[${v.map(canonical).join(",")}]`;
  return `{${Object.keys(v).sort().map((k) => `${JSON.stringify(k)}:${canonical(v[k])}`).join(",")}}`;
}
function append(log, signer, operation, subject) {
  const draft = createKeyEvent(log, { operation, subject, signerKeyId: signer.record.keyId });
  return { schema: 1, events: [...log.events, { ...draft, signature: { keyId: signer.record.keyId, host: signer.record.platform, value: sign("sha256", Buffer.from(canonical(draft.payload)), signer.privateKey).toString("base64") } }] };
}
const root = makeKey("windows", "Root");
const extra = makeKey("macos", "Second");
const first = append({ schema: 1, events: [] }, root, "root", root.record);
const second = append(first, root, "add", extra.record);
const third = append(second, root, "revoke", { keyId: extra.record.keyId });
const pin = root.record.keyId;
const baseline = { schema: 1, minimumScannerVersion: "1.0.0", minimumCatalogVersion: "1.0.0", revokedKeyIds: [], verifiedTrackingEvidence: [], repositories: [], provenance: [], hosts: {} };
const repo = (n) => ({ repositoryIdentity: `https://github.com/bogusyogi/${n}`, roots: ["."], visibility: "private" });
const manifest = { schema: 1, version: "4.0.0", capabilities: [{ package: "a", capability: "x", fingerprints: ["f1", "f2"] }, { package: "b", capability: "y", fingerprints: [] }] };
const inputs = { manifest, scannerVersion: "2.0.0", registry: { schema: 1, repositories: [repo("coderight")] } };
const bytesFor = (log) => Buffer.from(JSON.stringify({ schema: 1, log, rootFingerprint: pin, baseline, authority: undefined }));
const installedAt = (log, extraInputs = inputs) => summarizeTrustChange({ existing: null, requestBytes: Buffer.from(JSON.stringify({ schema: 1, log, rootFingerprint: pin, baseline })), ...extraInputs }).next;

test("summary lists repository additions, catalog and scanner floors, and key log sequence", () => {
  const existing = prepareTrustUpdate(null, { log: first, rootFingerprint: pin, baseline });
  const s = summarizeTrustChange({ existing, requestBytes: bytesFor(second), ...inputs });
  assert.equal(s.changed, true);
  const text = s.lines.join("\n");
  assert.match(text, /Repositories added \(1\): bogusyogi\/coderight/);
  assert.match(text, /Catalog: v1\.0\.0 none -> v4\.0\.0 [a-f0-9]{8} \(2 capabilities, 2 fingerprints/);
  assert.match(text, /Scanner floor: 1\.0\.0 -> 2\.0\.0/);
  assert.match(text, /Key log sequence: 0 -> 1/);
  assert.match(text, new RegExp(`Approval keys added: ${extra.record.keyId.slice(0, 12)} \\(macos\\)`));
  assert.match(text, /Payload SHA-256: [a-f0-9]{16}/);
  assert.equal(s.payloadDigest, createHash("sha256").update(bytesFor(second)).digest("hex"));
});

test("summary reports a catalog change against an installed digest", () => {
  const existing = installedAt(first);
  const s = summarizeTrustChange({ existing, requestBytes: bytesFor(first), ...inputs, manifest: { ...manifest, version: "5.0.0" } });
  const text = s.lines.join("\n");
  assert.match(text, /Catalog: v4\.0\.0 [a-f0-9]{8} -> v5\.0\.0 [a-f0-9]{8}/);
});

test("summary reports revoked approval keys", () => {
  const existing = installedAt(second);
  const s = summarizeTrustChange({ existing, requestBytes: bytesFor(third), ...inputs });
  assert.match(s.lines.join("\n"), new RegExp(`Approval keys revoked: ${extra.record.keyId.slice(0, 12)} \\(macos\\)`));
  assert.match(s.lines.join("\n"), /Key log sequence: 1 -> 2/);
});

test("no-op: identical trust reports already current", () => {
  const existing = installedAt(second);
  const s = summarizeTrustChange({ existing, requestBytes: bytesFor(second), ...inputs });
  assert.equal(s.changed, false);
  assert.deepEqual(s.lines, ["Ownership trust already current."]);
});

test("request file is digest-bound", () => {
  const dir = mkdtempSync(path.join(tmpdir(), "rk-sum-"));
  try {
    const file = path.join(dir, "r.json"); const bytes = bytesFor(second); writeFileSync(file, bytes);
    const digest = createHash("sha256").update(bytes).digest("hex");
    assert.equal(summarizeRequestFile({ file, digest, existing: installedAt(first), ...inputs }).changed, true);
    assert.throws(() => summarizeRequestFile({ file, digest: "0".repeat(64), existing: null, ...inputs }), /do not match pinned/);
  } finally { rmSync(dir, { recursive: true, force: true }); }
});

test("AppleScript text escaping handles quotes, backslashes and newlines", () => {
  assert.equal(appleScriptText('a"b\\c'), '"a\\"b\\\\c"');
  assert.equal(appleScriptText("l1\nl2\r\nl3"), '"l1" & return & "l2" & return & "l3"');
  const script = buildMacConfirmScript('x "q" \\ \n y');
  assert.match(script, /^display dialog "x \\"q\\" \\\\ " & return & " y" with title "RightKit ownership trust-sync" buttons \{"Cancel","Approve…"\} default button "Approve…" cancel button "Cancel" with icon caution$/);
});

test("capSummary truncates with an 'and N more' tail", () => {
  const out = capSummary(Array.from({ length: 40 }, (_, i) => `line ${i}`), { maxLines: 5 });
  assert.equal(out.split("\n").length, 5);
  assert.match(out, /…and 36 more$/);
});

test("Windows dialog uses the shown (interactive) runner, never the hidden one", () => {
  const used = [];
  confirmTrustChange({ lines: ["x"], nonce: "n1", run: () => { used.push("hidden"); return { status: 0 }; }, interactiveRun: (c, a, o) => { used.push(o.windowsHide); return { status: 0, stdout: "RK_CONFIRM_APPROVED:n1\r\n" }; }, print: () => {}, hostPlatform: "win32", tty: false });
  assert.deepEqual(used, [false]);
});

test("Windows MessageBox script is OKCancel Warning with the pinned title", () => {
  const ps = buildWindowsConfirmScript("a'b\nc");
  assert.match(ps, /MessageBoxW\(\[IntPtr\]::Zero, .*'RightKit ownership trust-sync', 0x50031\)/);
  assert.ok(ps.includes("'a''b', 'c'"));
});

test("confirm: macOS dialog cancel throws, approve returns; TTY requires yes", () => {
  const calls = [];
  const print = () => {};
  assert.throws(() => confirmTrustChange({ lines: ["x"], run: (c, a) => { calls.push([c, a]); return { status: 1, stderr: "execution error: User canceled. (-128)" }; }, print, hostPlatform: "darwin", tty: false }), /cancelled by user/);
  assert.throws(() => confirmTrustChange({ lines: ["x"], run: () => ({ status: 1, stderr: "execution error: No user interaction allowed. (-1713)" }), print, hostPlatform: "darwin", tty: false }), /could not be shown or read.*-1713/);
  assert.equal(calls[0][0], "/usr/bin/osascript");
  assert.throws(() => confirmTrustChange({ lines: ["x"], run: () => ({ status: 0, stdout: "" }), print, hostPlatform: "darwin", tty: false }), /could not be shown or read/);
  confirmTrustChange({ lines: ["x"], run: () => ({ status: 0, stdout: "button returned:Approve…, gave up:false\n" }), print, hostPlatform: "darwin", tty: false });
  assert.throws(() => confirmTrustChange({ lines: ["x"], run: () => assert.fail("no dialog on tty"), print, tty: true, readLine: () => "y" }), /cancelled by user/);
  confirmTrustChange({ lines: ["x"], run: () => assert.fail("no dialog on tty"), print, tty: true, readLine: () => "yes\n" });
  assert.throws(() => confirmTrustChange({ lines: ["x"], run: () => ({ status: 1 }), print, hostPlatform: "win32", tty: false }), /cancelled by user/);
  confirmTrustChange({ lines: ["x"], nonce: "abc", run: () => ({ status: 0, stdout: "RK_CONFIRM_APPROVED:abc\n" }), print, hostPlatform: "win32", tty: false });
});

test("Windows confirm approves ONLY on exit 0 plus the exact per-run token; kill/timeout/empty/wrong token cancel", () => {
  const confirm = (r) => confirmTrustChange({ lines: ["x"], nonce: "tok", run: () => r, print: () => {}, hostPlatform: "win32", tty: false });
  for (const r of [
    { status: 0 }, { status: 0, stdout: "" }, { status: 0, stdout: "RK_CONFIRM_APPROVED:other\n" },
    { status: 1, stdout: "RK_CONFIRM_APPROVED:tok\n" }, { status: null, signal: "SIGKILL", stdout: "" },
    { status: null, error: Object.assign(new Error("ETIMEDOUT"), { code: "ETIMEDOUT" }) }, { status: 0, stdout: "RK_CONFIRM_APPROVED:tok\n", error: new Error("x") }, undefined,
  ]) assert.throws(() => confirm(r), /cancelled by user/);
  confirm({ status: 0, stdout: "noise\r\nRK_CONFIRM_APPROVED:tok\r\n" });
});

test("Windows confirm script emits the token only in the OK branch", () => {
  const ps = buildWindowsConfirmScript("t", "tok");
  assert.match(ps, /if \(\$r -eq 1\) \{ \[Console\]::Out\.WriteLine\('RK_CONFIRM_APPROVED:tok'\); exit 0 \} else \{ exit 1 \}$/);
  assert.equal(ps.split("RK_CONFIRM_APPROVED").length, 2);
});

function gateFixture(existing, log, confirm) {
  const dir = mkdtempSync(path.join(tmpdir(), "rk-gate-"));
  const file = path.join(dir, "req.json"); const bytes = bytesFor(log); writeFileSync(file, bytes);
  const events = [];
  const d = { run: () => ({ status: 0 }), print: (t) => events.push(`print:${t}`), trustLoader: () => ({ trust: existing }), summaryInputs: inputs, confirm: confirm ?? (() => events.push("confirm")) };
  return { dir, request: { file, digest: createHash("sha256").update(bytes).digest("hex") }, d, events };
}

test("gate: no-op skips confirmation; cancel aborts before elevation; approve proceeds", async () => {
  const f1 = gateFixture(installedAt(second), second);
  try { assert.equal(await __testOnly.trustSyncGate(f1.d)(f1.request), false); assert.deepEqual(f1.events, []); } finally { rmSync(f1.dir, { recursive: true, force: true }); }

  const f2 = gateFixture(installedAt(first), second, () => { throw new Error("cancelled by user"); });
  try { assert.throws(() => __testOnly.trustSyncGate(f2.d)(f2.request), /cancelled by user/); } finally { rmSync(f2.dir, { recursive: true, force: true }); }

  const f3 = gateFixture(installedAt(first), second);
  try { assert.equal(await __testOnly.trustSyncGate(f3.d)(f3.request), true); assert.deepEqual(f3.events, ["confirm"]); } finally { rmSync(f3.dir, { recursive: true, force: true }); }
});

test("installCheckpoint: cancel never elevates; approve elevates once with the same digest; no-op skips", async () => {
  const mk = (existing, confirm) => { const f = gateFixture(existing, second, confirm); return f; };
  const run = async (existing, confirm) => {
    const calls = [];
    const f = mk(existing, confirm);
    f.d.elevate = (_r, file, digest) => { calls.push({ file, digest }); };
    f.d.gate = __testOnly.trustSyncGate(f.d);
    f.d.authority = undefined;
    let result, error;
    try { result = await __testOnly.installCheckpoint(second, pin, baseline, f.d); } catch (e) { error = e; }
    rmSync(f.dir, { recursive: true, force: true });
    return { calls, result, error };
  };
  const cancelled = await run(installedAt(first), () => { throw new Error("cancelled by user"); });
  assert.equal(cancelled.calls.length, 0); assert.match(cancelled.error.message, /cancelled by user/);
  const approved = await run(installedAt(first));
  assert.equal(approved.calls.length, 1); assert.equal(approved.result, true); assert.match(approved.calls[0].digest, /^[a-f0-9]{64}$/);
  const noop = await run(installedAt(second));
  assert.equal(noop.calls.length, 0); assert.equal(noop.result, false);
});

const allow = (extra = []) => ({ schema: 1, global: ["npm:typescript"], repositories: [{ repositoryIdentity: "https://github.com/bogusyogi/coderight", dependencies: ["cargo:serde", ...extra] }] });
const policyFor = (allowlist, files, scannerVersion = "2.0.0") => ({ allowlist, registry: inputs.registry, bundle: computePolicyBundle({ manifest, registry: inputs.registry, allowlist, scannerVersion, files }) });
const sourceFiles = (...pairs) => pairs.map(([name, digit]) => ({ path: name, sha256: digit.repeat(64) }));

test("summary announces the first policy-bundle pin and allowlist activation", () => {
  const existing = prepareTrustUpdate(null, { log: first, rootFingerprint: pin, baseline });
  const s = summarizeTrustChange({ existing, requestBytes: bytesFor(first), ...inputs, policy: policyFor(allow(), sourceFiles(["ownership.mjs", "1"], ["ownership-verdict.mjs", "2"])) });
  const text = s.lines.join("\n");
  assert.match(text, /Policy bundle: unpinned -> [a-f0-9]{12} \(pins 2 scanner source files, scanner 2\.0\.0, catalog, repository registry and dependency allowlist/);
  assert.match(text, /dependency allowlist: 1 global \+ 1 per-repository entries across 1 repositories \(first activation/);
  assert.deepEqual(s.next.dependencyAllowlist, allow());
  assert.equal(s.next.policyBundle.files.length, 2);
});

test("summary shows a semantic diff of bundle changes: scanner files, allowlist entries, registry and catalog", () => {
  const before = policyFor(allow(), sourceFiles(["ownership.mjs", "1"], ["ownership-verdict.mjs", "2"], ["old.mjs", "3"]));
  const existing = summarizeTrustChange({ existing: null, requestBytes: bytesFor(first), ...inputs, policy: before }).next;
  const after = policyFor(allow(["npm:newdep"]), sourceFiles(["ownership.mjs", "9"], ["ownership-verdict.mjs", "2"], ["fresh.mjs", "4"]), "2.1.0");
  const s = summarizeTrustChange({ existing, requestBytes: bytesFor(first), ...inputs, scannerVersion: "2.1.0", policy: after });
  const text = s.lines.join("\n");
  assert.equal(s.changed, true);
  assert.match(text, /Policy bundle: [a-f0-9]{12} -> [a-f0-9]{12}/);
  assert.match(text, /scanner version: 2\.0\.0 -> 2\.1\.0/);
  assert.match(text, /scanner\/verifier files changed \(1\): ownership\.mjs/);
  assert.match(text, /scanner\/verifier files added \(1\): fresh\.mjs/);
  assert.match(text, /scanner\/verifier files removed \(1\): old\.mjs/);
  assert.match(text, /allowlist bogusyogi\/coderight added \(1\): npm:newdep/);
  const registryChange = policyFor(allow(), sourceFiles(["ownership.mjs", "1"], ["ownership-verdict.mjs", "2"], ["old.mjs", "3"]));
  registryChange.bundle = computePolicyBundle({ manifest, registry: { schema: 1, repositories: [repo("other")] }, allowlist: allow(), scannerVersion: "2.0.0", files: before.bundle.files });
  assert.match(summarizeTrustChange({ existing, requestBytes: bytesFor(first), ...inputs, policy: registryChange }).lines.join("\n"), /repository registry file changed/);
});

test("summary is a no-op when the same bundle and allowlist are already pinned", () => {
  const policy = policyFor(allow(), sourceFiles(["ownership.mjs", "1"]));
  const existing = summarizeTrustChange({ existing: null, requestBytes: bytesFor(first), ...inputs, policy }).next;
  const again = summarizeTrustChange({ existing, requestBytes: bytesFor(first), ...inputs, policy });
  assert.equal(again.changed, false);
});
