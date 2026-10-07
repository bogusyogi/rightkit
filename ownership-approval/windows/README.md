# RightKit Windows ownership approval helper

Visible WinForms helper preserves legacy exception review/signing and adds Windows Hello key-log commands. Argumentless launch keeps existing review GUI.

## Native key-log commands

- `OwnershipApproval.exe --enroll --out <absolute-public.json>` calls `KeyCredentialManager.RequestCreateAsync` with `FailIfExists` once, then writes `{host:"windows",keyId:"win-<sha256-SPKI>",publicKeyPem}`. The credential uses dedicated name `rightkit.ownership.keylog.v1`; public output uses RSA-2048 SPKI. Existing credential and output paths are refused.
- `OwnershipApproval.exe --public-key --out <absolute-public.json>` opens existing Windows Hello credential for public-key read and writes the same record without signing or user authentication. Missing credentials fail.
- `OwnershipApproval.exe --key-operation <absolute-draft.json> --out <absolute-signed.json>` accepts only an unsigned `{schema:1,payload,signature:null}` add/revoke event with exact key-log schema. It validates RSA-2048 or P-256 SPKI fingerprints, requires `signerKeyId` to match the local Windows Hello key, then shows the full event and canonical payload SHA-256 in the main window. Press **Sign reviewed key-log event** to invoke `KeyCredential.RequestSignAsync` once. Signature envelope is `{keyId,host:"windows",value}` with a 256-byte RSA signature.
- `OwnershipApproval.exe --enroll-root --name <name> --out <absolute-event.json>` creates the same credential once through Windows Hello, builds the genesis event with local RSA key fingerprint, and saves it unsigned (`signature:null`) for administrator root pinning.

Windows Hello API owns per-use user verification for key-log signing & creation; command paths do not call the separate user-verification API or CNG key UI. There is no key rotation, arbitrary raw-sign interface, environment-selected key, trust-file modification, or authentication cache. Outputs use same-directory temporary files & exclusive atomic moves.

## Exception approvals

The existing exception-review GUI keeps schema-1 payload validation, full-payload review, digest display, explicit confirmation, and signed-record output. It prefers the dedicated Windows Hello RSA credential when available and signs through one `RequestSignAsync` call. Its signature envelope keeps the legacy `win-<sha256-SPKI>` key ID. If no Windows Hello credential exists, the existing nonexportable MPCP P-256 key & ForceHighProtection flow remains available.

## Build & source checks

Build on native Windows with workspace-provisioned .NET 10 SDK:

```powershell
$artifacts = Join-Path $env:TEMP 'rightkit-ownership-approval-artifacts'
$output = Join-Path $env:TEMP 'rightkit-ownership-approval-win-x64'
& 'C:\Users\adrds\.dotnet\dotnet.exe' publish tools/rightkit/ownership-approval/windows/OwnershipApproval.csproj -c Release -r win-x64 --artifacts-path $artifacts -o $output
```

Run source conformance with `powershell -NoProfile -File tools/rightkit/ownership-approval/windows/source-conformance.ps1`. Native build and ephemeral software-RSA interoperability do not enroll or authenticate a real key.
