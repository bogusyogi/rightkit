import assert from "node:assert/strict";
import test from "node:test";
import { PRODUCTS, repinText } from "./repin.mjs";

const sample = [
  "#requires -Version 5.1",
  "$Version = '0.3.4'",
  "$Uri = 'https://github.com/Orthic-Labs/legion/releases/download/v0.3.4/Legion-0.3.4-windows-x86_64-setup.exe'",
  "$ExpectedSha256 = 'aaaa'",
  "$ExpectedSigner = 'CN=Damned Ventures LLC'",
].join("\n");

test("repin rewrites exactly the three pinned literals", () => {
  const next = repinText(sample, {
    version: "0.3.12",
    uri: "https://github.com/Orthic-Labs/legion/releases/download/v0.3.12/Legion-0.3.12-windows-x86_64-setup.exe",
    digest: "b".repeat(64),
  });
  assert.match(next, /^\$Version = '0\.3\.12'$/m);
  assert.match(next, /releases\/download\/v0\.3\.12\/Legion-0\.3\.12-windows-x86_64-setup\.exe/);
  assert.ok(next.includes("$ExpectedSha256 = '" + "b".repeat(64) + "'"), "digest must be pinned verbatim");
  // The signer is an identity assertion, not a per-release pin: it must survive.
  assert.match(next, /\$ExpectedSigner = 'CN=Damned Ventures LLC'/);
  assert.doesNotMatch(next, /0\.3\.4/);
});

test("a bootstrap missing a pinned line fails loudly rather than half-updating", () => {
  const broken = sample.split("\n").filter((line) => !line.startsWith("$ExpectedSha256")).join("\n");
  assert.throws(() => repinText(broken, { version: "0.3.12", uri: "https://example.invalid/x", digest: "c".repeat(64) }), /ExpectedSha256/);
});

test("both branded products are declared with their release repositories", () => {
  assert.equal(PRODUCTS.legion.repo, "Orthic-Labs/legion");
  assert.equal(PRODUCTS.membrane.repo, "Orthic-Labs/Membrane");
  assert.equal(PRODUCTS.legion.assetPattern("0.3.12"), "Legion-0.3.12-windows-x86_64-setup.exe");
});
