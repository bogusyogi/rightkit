import assert from "node:assert/strict";
import test from "node:test";
import { assertWindowsVerification, WINDOWS_SIGNER_SUBJECT } from "./sign-windows.mjs";

// Shape/order follows Microsoft's verbose SignTool example (subjects/hashes are fixtures):
// https://learn.microsoft.com/en-us/windows-hardware/drivers/install/release-signing
const certificate = (subject, issuer, depth) => {
  const indent = "    ".repeat(depth);
  return [`Issued to: ${subject}`, `Issued by: ${issuer}`, "Expires:   Thu Dec 04 15:59:59 2036", "SHA1 hash: 0123456789ABCDEF0123456789ABCDEF01234567"]
    .map((line) => `${indent}${line}\n`).join("");
};
const signingChain = (leaf = "Damned Ventures LLC", intermediate = "Fixture Code Signing CA", root = "Fixture Root CA") =>
  `Signing Certificate Chain:\n${certificate(root, root, 1)}\n${certificate(intermediate, root, 2)}\n${certificate(leaf, intermediate, 3)}\n`;
const timestamp = (subject = "Fixture Timestamp Signer") =>
  `The signature is timestamped: Fri Oct 30 12:00:00 2026\nTimestamp Verified by:\n${certificate("Fixture Timestamp Root", "Fixture Timestamp Root", 1)}\n${certificate(subject, "Fixture Timestamp Root", 2)}\n`;
const report = (chain = signingChain(), stamp = timestamp()) =>
  `Verifying: fixture.exe\n${chain}${stamp}Successfully verified: fixture.exe\nNumber of files successfully Verified: 1\nNumber of warnings: 0\nNumber of errors: 0\n`;
const rejectedChain = (output) => assert.throws(() => assertWindowsVerification("fixture.exe", output), /signing certificate chain is missing, malformed or ambiguous/);

test("Windows verifier takes subject from signing leaf in root-to-leaf output", () => {
  const expected = { authenticode: "Valid", subject: WINDOWS_SIGNER_SUBJECT, timestampPresent: true };
  assert.deepEqual(assertWindowsVerification("fixture.exe", report()), expected);
  assert.deepEqual(assertWindowsVerification("fixture.exe", report().replace(/\n/g, "\r\n")), expected);
  assert.deepEqual(assertWindowsVerification("fixture.exe", report().replace(/    /g, "\t")), expected);
});

test("Windows verifier rejects matching issuer/root or timestamp subjects with wrong signing leaf", () => {
  for (const output of [
    report(signingChain("Someone Else", "Damned Ventures LLC")),
    report(signingChain("Someone Else", "Fixture Code Signing CA", "Damned Ventures LLC")),
    report(signingChain("Someone Else"), timestamp("Damned Ventures LLC")),
    report(signingChain("Damned Ventures LLC Fake")),
    report(signingChain("damned ventures llc")),
  ]) assert.throws(() => assertWindowsVerification("fixture.exe", output), /subject must be Damned Ventures LLC/);
});

test("Windows verifier rejects missing, malformed or ambiguous signing chains", () => {
  for (const output of [
    report(""),
    report("Signing Certificate Chain:\n"),
    report(signingChain() + signingChain()),
    report(signingChain().replace("Signing Certificate Chain:", "    Signing Certificate Chain:")),
    report(signingChain().replace("            Issued to: Damned Ventures LLC", "        Issued to: Damned Ventures LLC")),
    report(signingChain().replace("            Issued to: Damned Ventures LLC", "                Issued to: Damned Ventures LLC")),
    report(signingChain().replace("            Issued to: Damned Ventures LLC", "            Issued to:")),
    report(signingChain().replace("            Issued to: Damned Ventures LLC", "Issued to: Damned Ventures LLC")),
    report(signingChain().replace("            Issued to: Damned Ventures LLC", "            Issued too: Damned Ventures LLC")),
    report(signingChain() + certificate("Someone Else", "Fixture Code Signing CA", 3)),
  ]) rejectedChain(output);
});

test("Windows verifier still requires successful verification & timestamp evidence", () => {
  assert.throws(() => assertWindowsVerification("fixture.exe", report().replace("Successfully verified:", "Verification failed:")), /did not report Valid/);
  assert.throws(() => assertWindowsVerification("fixture.exe", report(signingChain(), "")), /timestamp is missing/);
});
