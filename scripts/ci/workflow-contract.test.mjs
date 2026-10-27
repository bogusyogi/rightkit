import assert from "node:assert/strict";
import test from "node:test";
import {
  ALL_JOBS,
  BUILD_CREDENTIALS_IF,
  CREDENTIALS_BUILD_RUN,
  CREDENTIALS_CLEANUP_RUN,
  CREDENTIALS_HELPER_ENV,
  CREDENTIALS_HELPER_SOURCE_COMMIT,
  CREDENTIALS_OK_OR_SKIPPED,
  CREDENTIALS_SEAL_RUN,
  CREDENTIALS_SIGN_RUN, CREDENTIALS_PROFILE_RUN, CREDENTIALS_RECORD_RUN, CREDENTIALS_SIGNER_RUNS,
  CREDENTIALS_VALIDATE_RUN,
  CREDENTIALS_VERIFY_STEP_IF,
  SIGN_CREDENTIALS_ARTIFACT,
  SIGN_CREDENTIALS_IF,
  SIGN_CREDENTIALS_SECRETS,
  SIGN_SWAP_ARTIFACT,
  SIGN_SWAP_SECRETS,
  UNSIGNED_CREDENTIALS_ARTIFACT,
  VERIFY_IF,
} from "./workflow-contract.mjs";

// Source-only pins for the credentials-helper contract extension: every value
// the workflow must carry exactly. The live proof (assertPublishDoc plus the
// mutation run) belongs to the integration owner, who runs the checks.
test("the qualification job list gains exactly the two credentials jobs", () => {
  assert.deepEqual(ALL_JOBS, ["guard", "scans", "plan", "build-swap", "sign-swap", "build-credentials-helper", "sign-credentials-helper", "pack", "verify", "verify-linux", "qualified"]);
});

test("credentials gating covers crates rightkit-credentials runs only", () => {
  assert.equal(BUILD_CREDENTIALS_IF, "${{ needs.plan.outputs.count != '0' && needs.plan.outputs.registry == 'crates' && contains(format(' {0} ', needs.plan.outputs.units), ' rightkit-credentials ') }}");
  assert.ok(SIGN_CREDENTIALS_IF.includes("needs.build-credentials-helper.result == 'success'"));
  assert.ok(SIGN_CREDENTIALS_IF.includes(BUILD_CREDENTIALS_IF.slice(3, -2)));
  assert.equal(CREDENTIALS_VERIFY_STEP_IF, "${{ needs.sign-credentials-helper.result == 'success' && runner.os == 'macOS' }}");
  assert.ok(VERIFY_IF.includes(CREDENTIALS_OK_OR_SKIPPED));
  assert.ok(CREDENTIALS_OK_OR_SKIPPED.includes("needs.build-credentials-helper.result == 'success'"));
  assert.ok(CREDENTIALS_OK_OR_SKIPPED.includes("needs.sign-credentials-helper.result == 'success'"));
  assert.ok(CREDENTIALS_OK_OR_SKIPPED.includes("needs.build-credentials-helper.result == 'skipped'"));
  assert.ok(CREDENTIALS_OK_OR_SKIPPED.includes("needs.sign-credentials-helper.result == 'skipped'"));
});

test("the credentials signer reuses the swap identity secrets plus exactly one profile secret", () => {
  for (const [key, value] of Object.entries(SIGN_SWAP_SECRETS)) {
    assert.equal(SIGN_CREDENTIALS_SECRETS[key], value);
  }
  assert.deepEqual(Object.keys(SIGN_CREDENTIALS_SECRETS), [...Object.keys(SIGN_SWAP_SECRETS), "APPLE_CREDENTIALS_QUAL_PROFILE_BASE64"]);
  assert.equal(SIGN_CREDENTIALS_SECRETS.APPLE_CREDENTIALS_QUAL_PROFILE_BASE64, "${{ secrets.APPLE_CREDENTIALS_QUAL_PROFILE_BASE64 }}");
  for (const value of Object.values(SIGN_CREDENTIALS_SECRETS)) {
    assert.match(value, /^\$\{\{ secrets\.[A-Z0-9_]+ \}\}$/);
  }
});

test("the sealed artifact names stay distinct from the swap route", () => {
  assert.equal(UNSIGNED_CREDENTIALS_ARTIFACT, "rightkit-credentials-helper-unsigned-${{ github.run_attempt }}");
  assert.equal(SIGN_CREDENTIALS_ARTIFACT, "rightkit-credentials-helper-signed");
  assert.notEqual(SIGN_CREDENTIALS_ARTIFACT, SIGN_SWAP_ARTIFACT);
});

test("the helper env reads the route from the job environment, never a literal", () => {
  assert.deepEqual(CREDENTIALS_HELPER_ENV, {
    RK_SOURCE_HELPER_APP: "${{ env.RK_SOURCE_HELPER_APP }}",
    RK_CONSUMER_HELPER_NAME: "${{ env.RK_CONSUMER_HELPER_NAME }}",
    RK_CONSUMER_HELPER_TEAM_ID: "${{ env.RK_CONSUMER_HELPER_TEAM_ID }}",
  });
});

test("the unsigned build pins the qualification sources and carries no secret", () => {
  assert.equal(CREDENTIALS_HELPER_SOURCE_COMMIT, "8f38ac109e09b182b0e5c0f4e365606a3522f45b");
  for (const blob of [
    "e9f6aff167f9219eab465fa4d87f9bcc992bee07",
    "19b29a3d71663736fe481873e4490c53d0c4f27a",
    "a4c53c429961eb187000a1f8151662ec4c529e26",
    "e1e3d9bc136c40ebccc13acb910f0bd009b56ca9",
    "fd45199d5ff0846712e05e4d833c9f60acd7a5b9",
  ]) {
    assert.ok(CREDENTIALS_BUILD_RUN.includes(blob), `build must verify blob ${blob}`);
  }
  for (const script of [CREDENTIALS_BUILD_RUN, CREDENTIALS_SEAL_RUN, CREDENTIALS_VALIDATE_RUN]) {
    assert.doesNotMatch(script, /\$\{\{/);
    assert.doesNotMatch(script, /secrets/i);
  }
});

test("the credentials signing run keeps the swap security bars plus profile binding", () => {
  for (const line of [
    "--options runtime",
    "TeamIdentifier=6KLGD3LLKF",
    "Identifier=org.rightkit.qualification.credentials",
    '{"keychain-access-groups": ["6KLGD3LLKF.org.rightkit.qualification.credentials"]}',
    "profile application-identifier mismatch",
    "expected.startswith(group[:-1])",
    "profile keychain-access-groups mismatch",
    "profile does not authorize the signing identity",
    "job=sign-credentials-helper",
  ]) {
    assert.ok(CREDENTIALS_SIGNER_RUNS.join("").includes(line), `the signer steps must keep ${line}`);
  }
  for (const run of CREDENTIALS_SIGNER_RUNS) {
    assert.doesNotMatch(run, /\$\{\{/);
    assert.ok(run.startsWith("set -euo pipefail\n"), "every split step keeps strict shell settings");
    // rhysd/actionlint#702: a run script over the ~4 KB Windows pipe buffer deadlocks actionlint's shellcheck integration.
    assert.ok(Buffer.byteLength(run) < 3800, `a signer step stays under 3800 bytes (${Buffer.byteLength(run)})`);
  }
  // Each secret or variable reaches only the step that uses it.
  assert.ok(CREDENTIALS_PROFILE_RUN.includes("$APPLE_CREDENTIALS_QUAL_PROFILE_BASE64") && !CREDENTIALS_PROFILE_RUN.includes("$APPLE_DEVELOPER_ID"));
  assert.ok(CREDENTIALS_SIGN_RUN.includes("$APPLE_DEVELOPER_ID") && !CREDENTIALS_SIGN_RUN.includes("$APPLE_CREDENTIALS_QUAL_PROFILE_BASE64"));
  assert.ok(!/\$APPLE_/.test(CREDENTIALS_RECORD_RUN));
  // Order kept: profile validated before signing, digest recorded after.
  assert.ok(CREDENTIALS_SIGN_RUN.includes('cp "$profile" "$out/RightKitCredentialsQualification.app/Contents/embedded.provisionprofile"'));
  assert.ok(CREDENTIALS_CLEANUP_RUN.includes("credentials-qual.mobileprovision"));
  // The whole profile validation stays in its own step, before any signing: every refusal it had in the single step.
  for (const refusal of ["profile application-identifier mismatch", "profile keychain-access-groups mismatch", "profile team mismatch", "profile platform mismatch", "profile expiry missing", "profile expired", "profile must disable debugging", "profile does not authorize the signing identity"]) {
    assert.ok(CREDENTIALS_PROFILE_RUN.includes(`raise SystemExit("${refusal}")`), `the profile step keeps "${refusal}"`);
  }
  assert.ok(/\[\[ "\$identity_hash" =~ \^\[0-9A-F\]\{40\}\$ \]\]/.test(CREDENTIALS_PROFILE_RUN), "the profile step requires a real identity hash");
  assert.doesNotMatch(CREDENTIALS_PROFILE_RUN, /(^|\n)codesign /, "nothing is signed before the profile is validated");
});
