# RightKit Windows ownership approval helper

This is RightKit's single Windows source for enrollment and signing. It is a
visible WinForms application with no signing command, command-line mode, or
environment-variable key selection. Start it as the Windows user who owns the
approval key.

## Enrollment

Press **Enroll this Windows host**. The helper first requests Windows user
verification, then creates one P-256 signing key in the Microsoft Platform
Crypto Provider with private-key export disabled and a native provider prompt
required for every use. It exports only a JSON enrollment record containing
the public SubjectPublicKeyInfo PEM, `host: "windows"`, and its
`win-<sha256>` key ID. Deliver that record to the administrator managing
the external ownership trust file. The helper never creates or edits that file,
changes trust ACLs, or rotates an existing key.

The Windows Hello check authenticates the currently signed-in Windows user.
The public-key entry in protected external trust is the authority that binds
this key to the Windows host and to Adrian; the helper's display name is not
an identity claim. No key attestation is collected by this helper.

The Microsoft Platform Crypto Provider is required; enrollment fails closed if
it is unavailable or the key does not retain the required export and prompt
policies. The helper also calls the Windows desktop
`UserConsentVerifierInterop.RequestVerificationForWindowAsync` API after the
visible enrollment/sign confirmation. Only a `Verified` result reaches key
creation or signing. The CNG provider prompt remains required in addition to
that Windows Hello/user-consent verification.

## Signing

Press **Review approval draft**, select an unsigned schema-1 record, and review
the entire displayed payload and its canonical SHA-256. Press **Sign reviewed
draft** only after review. The app re-reads the selected file and refuses a
changed payload, asks for a second explicit confirmation, requests Windows
user verification, then signs SHA-256(canonical JSON payload UTF-8) using
ECDSA P-256 in RFC 3279 DER format. The signature envelope matches
`packages/release/ownership-approval.mjs` (`host: "windows"`, key ID from
SHA-256 of DER SubjectPublicKeyInfo, base64 DER signature).

## Build and package

Build on the native Windows host with the workspace-provisioned .NET 10 SDK at
`C:\Users\adrds\.dotnet\dotnet.exe`. This project targets Windows build 22000
or later so the desktop user-verification API is available, and publishes a
self-contained x64 single-file GUI executable. Keep build artifacts and
published output outside the repository:

```powershell
$artifacts = Join-Path $env:TEMP 'rightkit-ownership-approval-artifacts'
$output = Join-Path $env:TEMP 'rightkit-ownership-approval-win-x64'
& 'C:\Users\adrds\.dotnet\dotnet.exe' publish tools/rightkit/ownership-approval/windows/OwnershipApproval.csproj -c Release -r win-x64 --artifacts-path $artifacts -o $output
```

The output directory must be outside app repositories. Keep this project and
its packaging source under RightKit; do not copy it into a product checkout.
This source package does not install files, modify protected OS paths, or
provision verifier trust. Native Windows compilation/linking succeeded with
.NET SDK 10.0.300. An isolated test invoked the compiled helper's canonical
JSON, digest, and key-ID functions, then verified an ephemeral software P-256
RFC 3279 DER signature with Node. It created no persistent key or approval
record. The OS verification and Microsoft Platform Crypto Provider prompts
have not been exercised; user-presence testing requires Adrian.

Run source conformance checks without compiling:

```powershell
powershell -NoProfile -File tools/rightkit/ownership-approval/windows/source-conformance.ps1
```

Microsoft API references:

- [Call Windows Runtime APIs in desktop apps](https://learn.microsoft.com/en-us/windows/apps/desktop/modernize/winrt-apis-desktop-apps)
- [UserConsentVerifier desktop interop](https://learn.microsoft.com/en-us/windows/apps/develop/ui/display-ui-objects)
- [RequestVerificationForWindowAsync](https://learn.microsoft.com/en-us/windows/win32/api/userconsentverifierinterop/nf-userconsentverifierinterop-iuserconsentverifierinterop-requestverificationforwindowasync)
- [CNG key UI protection levels](https://learn.microsoft.com/en-us/dotnet/api/system.security.cryptography.cnguiprotectionlevels)
- [Microsoft Platform Crypto Provider](https://learn.microsoft.com/en-us/windows/win32/seccertenroll/cng-key-storage-providers)
