# Ownership key enrollment preparation

This directory contains source drafts. Repository files are not trusted by the production verifier and must not be executed as an administrator for installation. The fixed verifier trust files are `C:\ProgramData\Orthic Labs\RightKit\ownership\trust.json` on Windows and `/Library/Application Support/Orthic Labs/RightKit/ownership/trust.json` on macOS. The release verifier audits those exact paths, every parent, ACL/owner, reparse points or symlinks, and fails closed when protection cannot be established. The installer repeats fixed-path protection checks. A same-agent-writable checkout is rejected; Adrian must first place reviewed installer source, schema module, and a protected Node runtime inside the administrator-protected installer directory.

## Human enrollment steps

Native builds are ready at these content-addressed development artifact paths:

- Windows: `D:\.rightkit-managed\ownership-approval\windows\content-529b1cffef8adfdfa9661ff2d6f15994c138b5b035bdfcc609a521d3fc627537\RightKit.OwnershipApproval.Windows.exe` (SHA-256 matches its directory suffix).
- Mac: `/Volumes/D/.rightkit-managed/ownership-approval/macos/content-f17d21e1ad78a789121c5ad7e9abfff7909a821628da3c28023568de4f5edea/ownership-approval --enroll` (SHA-256 matches its directory suffix).

These native development artifacts are locally built; their directories do not establish protected verifier authority. Windows compilation plus ephemeral signature interoperability passed; Mac compilation plus ephemeral signature interoperability passed. Adrian performs actual host enrollment from these visible native helpers.

On Windows, Adrian launches the native Windows helper as the Windows account that will own the key, presses **Enroll this Windows host**, completes Windows Hello verification and the Microsoft Platform Crypto Provider prompt, then saves the public JSON export. The helper exports public SPKI PEM only; private key export is disabled.

On Mac, Adrian launches the native helper with `--enroll` from the reviewed native build, presses **Enroll approval key…**, completes Touch ID or the Mac password prompt, then saves its public JSON export. The helper stores the private key in Secure Enclave and exports public SPKI PEM only. Do not run this until Adrian intentionally launches it on that Mac.

Copy only those two `.public.json` files to an administrator-controlled preparation host. Obtain the baseline trust JSON from an administrator-controlled copy of existing protected trust data. Do not replace an existing host key. If either platform already has an enrollment, stop for explicit rotation handling.

For a first installation with no protected registry, review `first-install-baseline.json` as a candidate policy before copying it into administrator-controlled preparation. Its nine repository identities were checked through GitHub on 2026-10-07: all are private, so no Actions workflows are installed. Whole-repository roots cover CodeRight's nested Membrane sources. It contains no keys, tracking authority or provenance exemptions; public CI registrations must be added through administrator review before use. It is never authoritative while in this checkout.

Required evidence is the original Windows and macOS public JSON exports, each showing its expected `host`, recomputable `keyId`, and public PEM; Adrian's direct confirmation that Windows Hello plus the CNG prompt completed on Windows, and Touch ID/Mac password completed on Mac; the administrator-reviewed baseline and candidate digests; and both displayed key fingerprints. Do not claim enrollment from build output, source conformance, or a generated test fixture.

## Build candidate

Run from the RightKit checkout with Node:

```powershell
node tools/rightkit/ownership-approval/prepare-enrollment.mjs --baseline <admin-trust.json> --windows <windows-public.json> --macos <macos-public.json> --out <candidate.json>
node --test tools/rightkit/ownership-approval/prepare-enrollment.test.mjs
```

The command accepts exactly the helper export fields, checks host labels, parses each public key as P-256, recomputes each host-prefixed key ID from SPKI DER, requires distinct keys, validates the trust registry schema, and carries repositories/roots/visibility, version floors, revocations, tracking evidence, and provenance through unchanged. It refuses to replace an existing output and writes a mode-0600 candidate. It never creates, imports, or signs a key and never writes protected trust.

The command prints SHA-256 of exact candidate file bytes, a canonical trust-object SHA-256, and both SPKI key fingerprints. Adrian must open and review the entire candidate against the protected baseline, record these values through an administrator-controlled channel, and stage that exact file digest in an administrator-protected location before OS install. The installer rereads the candidate once, checks the pinned raw-file digest, validates the complete registry and both host keys, retains those bytes, writes a same-directory temporary file, sets narrow file permissions, atomically replaces the fixed trust file, and verifies installed digest plus path protection. It never elevates itself or asks the OS to authenticate; Adrian launches it directly from an approved elevated Windows session or root Mac session. It refuses any installer source, schema module, or Node runtime outside the protected installer directory.

Windows protected installer directory: `C:\ProgramData\Orthic Labs\RightKit\ownership\installer`. Its script, schema module, and Node runtime must be regular files below this directory, owned by SYSTEM or Administrators, with write authority limited to those principals. Run:

```powershell
& 'C:\ProgramData\Orthic Labs\RightKit\ownership\installer\node.exe' 'C:\ProgramData\Orthic Labs\RightKit\ownership\installer\install-trust.mjs' --candidate '<reviewed-candidate.json>' --sha256 '<pinned-file-sha256>'
```

Before copying, Adrian creates `Orthic Labs\RightKit\ownership\installer` from an elevated Explorer session. Keep native ACLs on `C:\` and `C:\ProgramData`: the audit accepts their standard read grants and narrow Users create/attribute grants on these existing system directories. It also accepts the native `Authenticated Users` create-subdirectory grant on `C:\`; inherit-only generic grants are evaluated on each child when that child is inspected. Do not grant untrusted principals write, delete, DACL, or take-ownership rights on RightKit descendants. Set each created RightKit directory and installed file owner to `BUILTIN\Administrators` or `NT AUTHORITY\SYSTEM`, with write authority limited to SYSTEM and Administrators; Users may have read/execute access. Copy reviewed `node.exe`, `install-trust.mjs`, and `trust-schema.mjs` into `installer`. The installer checks each component and every ancestor through `C:\` before reading the candidate. It evaluates SID, numeric access masks, and propagation flags; create/attribute rights are accepted only on existing nonempty `C:\` and `C:\ProgramData` directories.

Mac protected installer directory: `/Library/Application Support/Orthic Labs/RightKit/ownership/installer`. Its script, schema module, and Node runtime must be regular root-owned files below this directory, not group/world writable, and have no ACL write grants except to `user:root`. Run:

```sh
sudo '/Library/Application Support/Orthic Labs/RightKit/ownership/installer/node' '/Library/Application Support/Orthic Labs/RightKit/ownership/installer/install-trust.mjs' --candidate '<reviewed-candidate.json>' --sha256 '<pinned-file-sha256>'
```

Provision on Mac from an administrator terminal after reviewing the runtime and source:

```sh
sudo install -d -o root -g wheel -m 755 '/Library/Application Support/Orthic Labs/RightKit/ownership/installer'
sudo install -o root -g wheel -m 755 '<reviewed-node-runtime>' '/Library/Application Support/Orthic Labs/RightKit/ownership/installer/node'
sudo install -o root -g wheel -m 644 '<reviewed-install-trust.mjs>' '/Library/Application Support/Orthic Labs/RightKit/ownership/installer/install-trust.mjs'
sudo install -o root -g wheel -m 644 '<reviewed-trust-schema.mjs>' '/Library/Application Support/Orthic Labs/RightKit/ownership/installer/trust-schema.mjs'
```

The installer expects its protected directory and files to have been provisioned by Adrian after reviewing their source and runtime. It does not bootstrap or loosen those permissions. It validates keys, IDs, registry roots, policy floors, revocations, evidence, and provenance before writing. It does not enroll keys or sign records.

Read-only host probes on 2026-10-07 found no existing `Orthic Labs\RightKit` Program Files/ProgramData install root on Windows and no `/Library/Application Support/Orthic Labs/RightKit` or helper app on Mac. Therefore Adrian must provision the installer directory from reviewed source and protected runtime on each native host before invoking it; do not elevate or execute this checkout to install. After install, run the installed verifier from a non-elevated session on each host. It must pass its fixed-path owner/ACL and link audit and report two distinct enrolled hosts before approvals can verify.
