import assert from "node:assert/strict";
import test from "node:test";
import { buildPresenceScript, presenceSummary, requireWindowsPresence, PRESENCE_APPROVE_PREFIX } from "./ownership-windows-presence.mjs";
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
  assert.match(ps, /CredUIPromptForWindowsCredentials/);
  assert.match(ps, /LogonUserW/);
  assert.equal(ps.split("RK_PRESENCE_VERIFIED").length, 2);
  assert.ok(ps.includes("'sum ''q'''"));
  assert.match(ps, /\$a -ne \$b\) \{ exit 1 \}/); // verified account must be the console user
});

test("summary is bounded and names old and new state", () => {
  const s = presenceSummary({ existing: { minimumCatalogDigest: "932449c1ffff", keyCheckpoint: { sequence: 3 }, repositories: [1] }, next: { minimumCatalogDigest: "a7121f96eeee", keyCheckpoint: { sequence: 4 }, repositories: [1, 2] } });
  assert.match(s, /932449c1 -> a7121f96/); assert.match(s, /3 -> 4/); assert.ok(s.length <= 900);
});

test("a denied presence check aborts the protected update before any write (win32 path)", { skip: process.platform !== "win32" }, () => {
  assert.throws(() => performProtectedTrustUpdate({}, { presence: () => { throw new Error("denied"); } }), /denied|elevated|root fingerprint/);
});
