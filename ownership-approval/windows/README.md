# RightKit Windows ownership approval helper

Visible WinForms helper reviews schema-2 exceptions & key-log commands through one Windows Hello RSA-2048 credential. Argumentless launch keeps existing review GUI.

## Native key-log commands

- `OwnershipApproval.exe --enroll --out <absolute-public.json>` calls `KeyCredentialManager.RequestCreateAsync` with `FailIfExists` once, then writes `{host:"windows",keyId:"win-<sha256-SPKI>",publicKeyPem}`. The credential uses dedicated name `rightkit.ownership.keylog.v1`; public output uses RSA-2048 SPKI. Existing credential and output paths are refused.
- `OwnershipApproval.exe --public-key --out <absolute-public.json>` opens existing Windows Hello credential for public-key read and writes the same record without signing or user authentication. Missing credentials fail.
- `OwnershipApproval.exe --key-operation <absolute-draft.json> --out <absolute-signed.json>` accepts only an unsigned `{schema:1,payload,signature:null}` add/revoke event with exact key-log schema. It validates RSA-2048 or P-256 SPKI fingerprints, requires `signerKeyId` to match the local Windows Hello key, then shows the full event and canonical payload SHA-256 in the main window. Press **Sign reviewed key-log event** to invoke `KeyCredential.RequestSignAsync` once. Signature envelope is `{keyId,host:"windows",value}` with a 256-byte RSA signature.
- `OwnershipApproval.exe --enroll-root --name <name> --out <absolute-event.json>` creates the same credential once through Windows Hello, builds the genesis event with local RSA key fingerprint, and saves it unsigned (`signature:null`) for administrator root pinning.

Windows Hello API owns per-use user verification for key-log signing & creation; command paths do not call the separate user-verification API or CNG key UI. There is no key rotation, arbitrary raw-sign interface, environment-selected key, trust-file modification, or authentication cache. Outputs use same-directory temporary files & exclusive atomic moves.

## Exception approvals

Exception-review GUI requires [schema v2](../../docs/OWNERSHIP-EXCEPTION-SCHEMA-V2.md): exact payload keys, fingerprint ID regex, policy digest, dotted-numeric scanner/catalog floors, 1–256 distinct path/hash occurrences, issued/expiry dates at most 90 days apart, & verified HTTPS tracking without userinfo. Full-payload review, digest display & explicit confirmation precede one `RequestSignAsync` call. Enrollment, staging, export & signing all use dedicated Hello RSA credential; missing credentials fail closed. Exception signature IDs remain `win-<sha256-SPKI>`; key-log IDs use bare SPKI fingerprints.

## Build & source checks

Build on native Windows with workspace-provisioned .NET 10 SDK:

```powershell
$artifacts = Join-Path $env:TEMP 'rightkit-ownership-approval-artifacts'
$output = Join-Path $env:TEMP 'rightkit-ownership-approval-win-x64'
& 'C:\Users\adrds\.dotnet\dotnet.exe' publish rightkit/ownership-approval/windows/OwnershipApproval.csproj -c Release -r win-x64 --artifacts-path $artifacts -o $output
```

Run source conformance with `node --test rightkit/ownership-approval/conformance/native-source-conformance.test.mjs`. From workspace root, this checks both native sources on Windows or macOS. It pins native refusal guards, extracts payload keys/regexes & checks every shared `conformance/v2-vectors.json` draft through source-projected rules without compilation or authentication. Native build & ephemeral software-RSA interoperability do not enroll or authenticate a real key.
