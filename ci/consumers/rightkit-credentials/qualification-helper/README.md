# Isolated credentials qualification helper

Fixture only: `RightKitCredentialsQualification.app`, identifier/service `org.rightkit.qualification.credentials`, access group `6KLGD3LLKF.org.rightkit.qualification.credentials`, account `qualification.synthetic`. Wrapper accepts only status with no scope/account/value; Rust consumer sends `{"command":"status"}` (`crates/rightkit-credentials/src/apple_helper.rs`, `AppleHelper::status`). Status probes synthetic service/account `status` through production backend without requesting secret data. No application credential namespace is used.

Root integration owns native compilation, signing & execution. Commands below use committed RightKit Swift target sources solely for isolated qualification artifact; they create no application dependency or production package change. Existing published Swift 0.8.1 lacks required host API.

## Exact source binding

Source commit: `8f38ac109e09b182b0e5c0f4e365606a3522f45b`, prefix `swift/RightKitSwift/Sources/RightKitAppleCredentials/`. Git blob IDs bind exact committed bytes, independent of checkout line endings:

| File | Git blob SHA-1 |
|---|---|
| AppleCredentialHelperHost.swift | e9f6aff167f9219eab465fa4d87f9bcc992bee07 |
| ForeignGenericPassword.swift | 19b29a3d71663736fe481873e4490c53d0c4f27a |
| RightKitAppleCredentials.swift | a4c53c429961eb187000a1f8151662ec4c529e26 |
| SyncedCredentialPolicy.swift | e1e3d9bc136c40ebccc13acb910f0bd009b56ca9 |
| SyncedEnvelopeStore.swift | fd45199d5ff0846712e05e4d833c9f60acd7a5b9 |

## Native root invocation

Run on provisioned Mac from RightKit primary checkout. Set `SCRATCH` to fresh absolute directory under `/Volumes/D/scratch`; parent must already exist. Export `FIXTURE` from this checkout. Build uses static library, so staged helper has no private dylib dependency.

Set `PROFILE` to issued `MAC_APP_DIRECT` profile for this identifier (bundle record `6A3SLN3NW3`, profile `W7AVF2AK9C`, produced by `packages/release/apple-profile-create.mjs`). Profile embeds at `Contents/embedded.provisionprofile` before signing, so codesign seals it. Signing without it still passes preparer signature & entitlement-shape checks yet leaves keychain-access-group unauthorized at runtime: readiness then fails. Before embedding, decode profile (`security cms -D`) & require application-identifier `6KLGD3LLKF.org.rightkit.qualification.credentials`, authorization for `6KLGD3LLKF.org.rightkit.qualification.credentials`, team `6KLGD3LLKF`, OSX platform, future expiry, debugging disabled & embedded Developer ID certificate fingerprint matching signing identity. Issued profile authorizes team wildcard `6KLGD3LLKF.*`; signed helper must still request exactly `[6KLGD3LLKF.org.rightkit.qualification.credentials]`, never that wildcard. ASC Admin key stays on provisioned Mac; profile bytes alone carry no private key.

```sh
set -eu
export FIXTURE="$PWD/ci/consumers/rightkit-credentials/qualification-helper"
export SCRATCH=/Volumes/D/scratch/credentials-qualification-helper
mkdir -m 700 "$SCRATCH"
python3 - <<'PY'
import hashlib, json, os, subprocess
from pathlib import Path
root = Path(os.environ['SCRATCH'])
source = root / 'source'
source.mkdir(mode=0o700)
commit = '8f38ac109e09b182b0e5c0f4e365606a3522f45b'
blobs = {
    'AppleCredentialHelperHost.swift': 'e9f6aff167f9219eab465fa4d87f9bcc992bee07',
    'ForeignGenericPassword.swift': '19b29a3d71663736fe481873e4490c53d0c4f27a',
    'RightKitAppleCredentials.swift': 'a4c53c429961eb187000a1f8151662ec4c529e26',
    'SyncedCredentialPolicy.swift': 'e1e3d9bc136c40ebccc13acb910f0bd009b56ca9',
    'SyncedEnvelopeStore.swift': 'fd45199d5ff0846712e05e4d833c9f60acd7a5b9',
}
records = []
for name, expected in blobs.items():
    data = subprocess.check_output(['git', 'show', commit + ':swift/RightKitSwift/Sources/RightKitAppleCredentials/' + name])
    actual = hashlib.sha1(b'blob ' + str(len(data)).encode() + b'\0' + data).hexdigest()
    if actual != expected:
        raise SystemExit('source blob mismatch: ' + name)
    (source / name).write_bytes(data)
    records.append({'name': name, 'gitBlob': actual, 'sha256': hashlib.sha256(data).hexdigest()})
for name in ['main.swift', 'Info.plist', 'entitlements.plist']:
    data = (Path(os.environ['FIXTURE']) / name).read_bytes()
    (source / name).write_bytes(data)
    records.append({'name': name, 'sha256': hashlib.sha256(data).hexdigest()})
(root / 'source-inputs.json').write_text(json.dumps({'sourceCommit': commit, 'files': records}, indent=2) + '\n')
PY
mkdir "$SCRATCH/module-cache"
mkdir -p "$SCRATCH/RightKitCredentialsQualification.app/Contents/MacOS"
export CLANG_MODULE_CACHE_PATH="$SCRATCH/module-cache"
xcrun swiftc -swift-version 6 -target "$(uname -m)-apple-macosx14.0" \
  -module-cache-path "$SCRATCH/module-cache" -parse-as-library \
  -emit-library -static -emit-module -module-name RightKitAppleCredentials \
  -emit-module-path "$SCRATCH/RightKitAppleCredentials.swiftmodule" \
  "$SCRATCH/source/AppleCredentialHelperHost.swift" \
  "$SCRATCH/source/ForeignGenericPassword.swift" \
  "$SCRATCH/source/RightKitAppleCredentials.swift" \
  "$SCRATCH/source/SyncedCredentialPolicy.swift" \
  "$SCRATCH/source/SyncedEnvelopeStore.swift" \
  -o "$SCRATCH/libRightKitAppleCredentials.a"
xcrun swiftc -swift-version 6 -target "$(uname -m)-apple-macosx14.0" \
  -module-cache-path "$SCRATCH/module-cache" -I "$SCRATCH" -L "$SCRATCH" \
  -lRightKitAppleCredentials "$SCRATCH/source/main.swift" \
  -o "$SCRATCH/RightKitCredentialsQualification.app/Contents/MacOS/RightKitCredentialsQualification"
cp "$SCRATCH/source/Info.plist" "$SCRATCH/RightKitCredentialsQualification.app/Contents/Info.plist"
cp "$PROFILE" "$SCRATCH/RightKitCredentialsQualification.app/Contents/embedded.provisionprofile"
node packages/release/sign-macos.mjs \
  --entitlements "$SCRATCH/source/entitlements.plist" \
  --receipt "$SCRATCH/signing.json" \
  "$SCRATCH/RightKitCredentialsQualification.app"
```

Run shell with `set -eu` so failures stop before signing/staging. Preserve source-inputs, compiler output, signing receipt & helper tree digest. Set `RK_SOURCE_HELPER_APP` to resulting app, `RK_CONSUMER_HELPER_NAME=RightKitCredentialsQualification` & `RK_CONSUMER_HELPER_TEAM_ID=6KLGD3LLKF`; use parent preparer with fresh consumer bundle/receipt paths. Preparer independently checks designated identifier, team, exact sole access group & copied bytes. Run staged consumer's `native-helper` mode for readiness & wrong-team refusal evidence.

## CI provisioning handoff (root-owned)

`publish.yml` currently provisions none of `RK_SOURCE_HELPER_APP`, `RK_CONSUMER_HELPER_NAME` or `RK_CONSUMER_HELPER_TEAM_ID`, so crates verification of `rightkit-credentials` refuses at `planNativeHelper`/`signedHelper` prerequisites before Cargo work; never weaken that refusal. Root mirrors `build-swap`/`sign-swap`: secretless `xcode-27` build job compiles unsigned app from pinned blobs above & seals source-inputs/tree digest/run receipt; fresh `release`-environment sign job (no checkout, install or compile) validates same-run artifact before credential import, decodes profile from release-environment secret, re-validates exact profile fields above, embeds it, Developer ID signs with committed entitlements (carried hash-sealed in unsigned artifact), verifies designated requirement & sole group, seals signed tree digest; macOS `verify` downloads that same-run signed artifact, re-checks receipt & tree digest, then exports three variables above. No ASC key, profile regeneration or notarization in CI; expired profile fails qualification until root renews secret locally. Workflow, contract & inventory-hash changes belong root.
