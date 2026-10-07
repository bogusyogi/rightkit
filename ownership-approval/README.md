# Ownership key enrollment preparation

This directory contains source drafts. Repository files are not trusted by the production verifier and must not be executed as an administrator for installation. The fixed verifier trust files are `C:\ProgramData\Orthic Labs\RightKit\ownership\trust.json` on Windows and `/Library/Application Support/Orthic Labs/RightKit/ownership/trust.json` on macOS. The release verifier audits those exact paths, every parent, ACL/owner, reparse points or symlinks, and fails closed when protection cannot be established. The installer repeats fixed-path protection checks. A same-agent-writable checkout is rejected; Adrian must first place reviewed installer source, schema module, and a protected Node runtime inside the administrator-protected installer directory.

## Human enrollment steps

Native builds are ready at these content-addressed development artifact paths:

- Windows: `D:\.rightkit-managed\ownership-approval\windows\content-529b1cffef8adfdfa9661ff2d6f15994c138b5b035bdfcc609a521d3fc627537\RightKit.OwnershipApproval.Windows.exe` (SHA-256 matches its directory suffix).
- Mac: `/Users/adrdsouza/.rightkit-managed/ownership-approval/macos/content-f17d21e1ad78a789121c5ad7e9abfff7909a821628da3c28023568de4f5edea0/ownership-approval --enroll` (SHA-256 matches its directory suffix).

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

## Prepared rollout bundles

Exact source from commit `b22fdf2998333462e62192cd643442168c904013` is staged with existing Node 26.8.1 runtimes & regular-file `smol-toml` dependencies. Mac bundles use home storage because `/Volumes/D` became unavailable. These are review inputs; directory hashes do not establish protected authority.

| Host | Bundle | Absolute development path |
|---|---|---|
| Windows | Adapters | `D:\.rightkit-managed\ownership-install\windows-adapters-7944c2f34654d5dcbb7780e6d97ddef7d57bce8c920cf793136e11a544a7db93` |
| Windows | Trust installer | `D:\.rightkit-managed\ownership-install\windows-installer-5fec910d0736f97a7558bacb68a50741e810a51045ea67027aff7e9a7bd016ba` |
| Windows | Baseline policy | `D:\.rightkit-managed\ownership-install\windows-candidate-671c540e539d150ebbe8207c6701004fe97639b3b0b1aaab20a8c4c24ffdf95d` |
| Mac | Adapters | `/Users/adrdsouza/.rightkit-managed/ownership-install/macos-adapters-5a5c3ab010ecc759a547fcb4d5b501f9567a1498b90ac28c0c4d9688327ceefa` |
| Mac | Trust installer | `/Users/adrdsouza/.rightkit-managed/ownership-install/macos-installer-a6f956867a8d1c3b2b78d215f436a943132f85be599259321ef300d5ca4c6e1b` |
| Mac | Baseline policy | `/Users/adrdsouza/.rightkit-managed/ownership-install/macos-candidate-671c540e539d150ebbe8207c6701004fe97639b3b0b1aaab20a8c4c24ffdf95d` |

Each adapter bundle has 26 regular files. Its `ownership/adapters/adapter-files.json` SHA-256 is `59a0afc55576a6d68fac9b19a3f3f72828cde3f19d5a48c4301acea842d8a908` on Windows & `f26ebcde7455ce8c7d7434f85666fae6bcb8bbef9de2d0c50198b15c4928649a` on Mac. Manifest verification, production imports, syntax, development-path rejection, & enrollment policy lifecycle checks passed on both hosts. Baseline `first-install-baseline.json` has SHA-256 `83c8d1fa1cf5a8408ef5aa2569e7185f71817e1cdcdc1de8a0af675633b05cda` & zero host keys; use it only as reviewed baseline input to `prepare-enrollment.mjs` after obtaining both public exports.

For first install, copy adapter bundle contents into fresh fixed adapter roots listed in [adapters/README.md](adapters/README.md). Copy `ownership/installer/` from each trust-installer bundle into its fixed protected installer directory described above. Refuse existing destinations, preserve existing trust during upgrades, & apply required owner/ACL protection to every new ancestor & installed file before executing. On Mac, `ditto` can preserve source ownership: explicitly set copied tree owner to `root:wheel` & remove group/world write; retain restrictive key permissions. Verify copied code inventory with installed `package-manifest.mjs --verify --root <fixed-adapter-root>`. Enrollment policy/installer paths have separate protection checks, so adding reviewed trust leaves code inventory valid.

Existing duplicates have 102 unsigned migration drafts; complete real expiry, request ID, & protected tracking evidence, then use Adrian's native review/sign/import flow in [migrations/README.md](migrations/README.md).
