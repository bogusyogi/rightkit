import assert from "node:assert/strict";
import test from "node:test";
import { authBadge } from "../src/lib/auth.ts";

test("authBadge: null when no auth data", () => {
  assert.equal(authBadge(null), null);
});

test("authBadge: DMARC pass → Verified", () => {
  const b = authBadge({ spf: "pass", dkim: "pass", dmarc: "pass" });
  assert.equal(b?.level, "pass");
  assert.equal(b?.label, "Verified");
});

test("authBadge: a fail without DMARC pass → Failed auth", () => {
  const b = authBadge({ spf: "fail", dkim: "none", dmarc: "fail" });
  assert.equal(b?.level, "fail");
});

test("authBadge: SPF pass alone (no DMARC) → Verified", () => {
  const b = authBadge({ spf: "pass", dkim: "none", dmarc: "none" });
  assert.equal(b?.level, "pass");
});

test("authBadge: all none → no badge (inconclusive)", () => {
  assert.equal(authBadge({ spf: "none", dkim: "none", dmarc: "none" }), null);
});

test("authBadge: DMARC pass overrides an SPF fail (forwarded mail)", () => {
  const b = authBadge({ spf: "fail", dkim: "pass", dmarc: "pass" });
  assert.equal(b?.level, "pass");
});
