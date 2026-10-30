// GB-14 (GATE 37/E11): the gate's own qualify record is a journal of the protected installed-QA receipt, never a coarser claim beside it. The record
// carries the receipt's digest; reusing the record calls the EXISTING protected verifier (release-qa-receipt.mjs) over the exact current state, so a
// changed policy, floor, check coverage, source, artifact hash or sealed byte invalidates it at once. No parallel qualification framework exists here.
import fs from "node:fs";
import { QaAdmissionError, assertInstalledQaAdmission, qaReceiptPath, readQaReceipt } from "./release-qa-receipt.mjs";

const HEX64 = /^[0-9a-f]{64}$/;

/** The digest of the installed-QA receipt the qualify stage just produced for `releaseId`, or null when none is readable. */
export function installedQaReceiptDigest({ repoRoot, releaseId, platform, readReceipt = readQaReceipt }) {
  const receipt = readReceipt(qaReceiptPath({ repoRoot, releaseId, platform, stage: "installed" }));
  return receipt && HEX64.test(receipt.digest ?? "") ? receipt.digest : null;
}

/**
 * Why a recorded qualify result no longer stands for the sealed bytes now, or null when it does. `verify` defaults to the existing protected admission
 * (assertInstalledQaAdmission over the sealed manifest); it needs this host's receipt key, so a runner that has none (the trusted publisher, whose
 * upload admission verifies the receipt itself) passes `verify: null` and is held to the digest equality and the manifest digest only.
 */
export function qualifyReuseProblem({ recorded, sealedNow, repoRoot, platform, configName, verify = defaultVerify, readFile = (f) => fs.readFileSync(f, "utf8") }) {
  if (!recorded?.ok) return "no passed qualify result is recorded";
  if (!sealedNow || recorded.manifestSha256 !== sealedNow.digest) return "the sealed bytes differ from the ones qualified";
  if (!HEX64.test(recorded.qaReceiptDigest ?? "")) return "the qualify record carries no protected QA receipt digest";
  if (verify === null) return null;
  try {
    const admitted = verify({ repoRoot, sealedNow, platform, configName, readFile });
    if (admitted?.digest !== recorded.qaReceiptDigest) return "the protected QA receipt on disk is not the one the qualify record names";
  } catch (error) {
    if (error instanceof QaAdmissionError) return `the protected installed-QA receipt no longer verifies: ${error.message}`;
    return `the protected installed-QA receipt could not be verified: ${error.message}`;
  }
  return null;
}

function defaultVerify({ repoRoot, sealedNow, platform, configName, readFile }) {
  const manifest = JSON.parse(readFile(sealedNow.manifest));
  return assertInstalledQaAdmission({ repoRoot, manifest, platform, configName });
}
