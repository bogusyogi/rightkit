import assert from "node:assert/strict";
import test from "node:test";
import { buildPresenceScript, presenceHelloMessage, presenceSummary, requireWindowsPresence, PRESENCE_APPROVE_PREFIX } from "./ownership-windows-presence.mjs";
import { performProtectedTrustUpdate } from "./ownership-key-trust.mjs";

const ok = (nonce) => ({ status: 0, stdout: `${PRESENCE_APPROVE_PREFIX}${nonce}\r\n` });

test("presence approves only on exit 0 plus the exact per-run token", () => {
  assert.equal(requireWindowsPresence({ summary: "s", nonce: "n", run: () => ok("n") }), true);
  for (const r of [
    undefined, { status: 0 }, { status: 0, stdout: "" }, { status: 0, stdout: ok("other").stdout }, { status: 1, stdout: ok("n").stdout },
    { status: 3, stdout: "" }, { status: null, signal: "SIGKILL" }, { status: null, error: Object.assign(new Error("t"), { code: "ETIMEDOUT" }) },
    { status: 0, stdout: ok("n").stdout, error: new Error("x") },
  ]) assert.throws(() => requireWindowsPresence({ summary: "s", nonce: "n", run: () => r }), /RK_OWNERSHIP_PRESENCE_DENIED/);
});

test("presence runs shown, with a kill-capable timeout", () => {
  let seen;
  requireWindowsPresence({ summary: "s", nonce: "n", timeoutMs: 1234, run: (c, a, o) => { seen = o; return ok("n"); } });
  assert.equal(seen.windowsHide, false); assert.equal(seen.timeout, 1234); assert.equal(seen.killSignal, "SIGKILL");
});

test("presence script refuses non-interactive sessions before prompting and prints the token on one path only", () => {
  const ps = buildPresenceScript("sum 'q'", "tok");
  assert.ok(ps.indexOf("UserInteractive") < ps.indexOf("Verify("));
  assert.ok(ps.indexOf("SessionId") < ps.indexOf("Verify("));
  assert.ok(ps.indexOf("UserInteractive") < ps.indexOf("RequestForWindow($form"));
  assert.match(ps, /CredUIPromptForWindowsCredentials/);
  assert.match(ps, /LogonUserW/);
  assert.equal(ps.split("RK_PRESENCE_VERIFIED").length, 2);
  assert.ok(ps.includes("'sum ''q'''"));
  assert.match(ps, /\$a -ne \$consoleSid\) \{ exit 1 \}/); // verified account must be the console user
});

test("exit codes map: Canceled/DeviceBusy/NotConfigured/RetriesExhausted/DisabledByPolicy cancel; 2 error and 3 non-interactive deny", () => {
  for (const status of [1, 2, 3]) assert.throws(() => requireWindowsPresence({ summary: "s", nonce: "n", run: () => ({ status, stdout: "" }) }), /RK_OWNERSHIP_PRESENCE_DENIED/);
});

test("presence script approves Hello only on Verified, binds to the console user, shows an owner window, and falls back to password when unavailable", () => {
  const ps = buildPresenceScript("x\nCatalog: a -> b\nKey log sequence: 1 -> 2\nRepositories: 1 -> 2\nz", "tok");
  assert.match(ps, /UserConsentVerifier\]::CheckAvailabilityAsync/);
  assert.match(ps, /\$avail -eq 'Available'/);
  assert.match(ps, /\$res -ne 'Verified'\) \{ exit 1 \}/);
  assert.ok(ps.indexOf("$res -ne 'Verified'") < ps.indexOf("RK_PRESENCE_VERIFIED"));
  assert.ok(ps.indexOf("$avail -eq 'Available'") < ps.indexOf("[RkPresence]::Verify("));
  assert.match(ps, /RequestVerificationForWindowAsync/);
  assert.match(ps, /TopMost=\$true/);
  assert.match(ps, /WindowsIdentity\]::GetCurrent\(\)\.User\.Value -ne \$consoleSid\) \{ exit 1 \}/);
  assert.ok(ps.includes("RightKit ownership trust-sync: Catalog: a -> b; Key log sequence: 1 -> 2; Repositories: 1 -> 2"));
  assert.equal(ps.split("RK_PRESENCE_VERIFIED").length, 2);
});

test("hello message is one bounded line carrying the payload digest", () => {
  const m = presenceHelloMessage(presenceSummary({ existing: { minimumCatalogDigest: "932449c1ffff" }, next: { minimumCatalogDigest: "a7121f96eeee" } }));
  assert.match(m, /^RightKit ownership trust-sync: /); assert.match(m, /a7121f96/); assert.ok(!m.includes("\n")); assert.ok(m.length <= 200);
});

test("summary is bounded and names old and new state", () => {
  const s = presenceSummary({ existing: { minimumCatalogDigest: "932449c1ffff", keyCheckpoint: { sequence: 3 }, repositories: [1] }, next: { minimumCatalogDigest: "a7121f96eeee", keyCheckpoint: { sequence: 4 }, repositories: [1, 2] } });
  assert.match(s, /932449c1 -> a7121f96/); assert.match(s, /3 -> 4/); assert.ok(s.length <= 900);
});

test("a denied presence check aborts the protected update before any write (win32 path)", { skip: process.platform !== "win32" }, () => {
  assert.throws(() => performProtectedTrustUpdate({}, { presence: () => { throw new Error("denied"); } }), /denied|elevated|root fingerprint/);
});
