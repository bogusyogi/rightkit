# Ownership enrollment

Run these commands from primary clones of this repository. Helpers export public keys only. Commands create directories, install protected trust, commit key metadata & share it through configured Git origin automatically. No peer SSH, copied runtime, installer directory, candidate digest or standalone checkpoint sync is needed.

## First machine

Windows first:

```powershell
pnpm --dir rightkit exec node packages/release/cli/right-release.mjs ownership enroll --root --name Windows
```

Or Mac first:

```sh
pnpm --dir rightkit exec node packages/release/cli/right-release.mjs ownership enroll --root --name Mac
```

One native authentication creates this machine's non-exportable key; one UAC/sudo installs its public fingerprint & genesis checkpoint. Command prints root fingerprint & active-key count. Windows uses Windows Hello RSA-2048; Mac uses Secure Enclave P-256 with Touch ID/password. Windows root genesis is authorized by protected root pin/checkpoint; Mac root genesis also carries its self-signature. Every later add/revoke needs an active key's signature.

## Each additional machine

On Mac, when Windows enrolled first:

```sh
pnpm --dir rightkit exec node packages/release/cli/right-release.mjs ownership enroll --name Mac --root-fingerprint <root-fingerprint-printed-by-first-machine>
```

On Windows, when Mac enrolled first:

```powershell
pnpm --dir rightkit exec node packages/release/cli/right-release.mjs ownership enroll --name Windows --root-fingerprint <root-fingerprint-printed-by-first-machine>
```

One native authentication creates this device's key & public request; at most one elevation pins known root & current checkpoint. An unsigned request grants no authority. Enrollment refuses duplicate keys & root replacement. Re-running after cancelled elevation resumes only matching committed metadata & existing public key, without creating or authenticating another key.

On any enrolled machine, approve printed device fingerprint:

```sh
pnpm --dir rightkit exec node packages/release/cli/right-release.mjs ownership approve-key <new-device-fingerprint>
```

Review full event in native helper & authorize once. At most one elevation advances protected checkpoint as part of this command. Entire ceremony: one command on new device, one approve-key on enrolled device.

## Lost-device revocation

On any remaining enrolled machine:

```sh
pnpm --dir rightkit exec node packages/release/cli/right-release.mjs ownership revoke-key <lost-device-fingerprint>
```

One native authentication signs revocation; at most one elevation advances checkpoint. Revoked keys cannot issue new events or approve exceptions. Their valid historical additions remain valid. Root may be revoked as active signer while its public fingerprint remains historical trust anchor.

## Trust & offline use

Committed `.rightkit/ownership-keys.json` in this shared workspace repository supports N devices. Public requests live in `.rightkit/key-requests/`. Enrollment automatically pins this repository's canonical identity & local checkout path as shared authority; independent product repositories read its committed key list without copying it. Evidence binds authority identity, commit/tree, log digest & checkpoint. Dirty authority worktree files cannot supply keys.

Root fingerprint, authority location & checkpoint remain outside repository at `C:\ProgramData\Orthic Labs\RightKit\ownership\trust.json` or `/Library/Application Support/Orthic Labs/RightKit/ownership/trust.json`. Installer sets Administrators/SYSTEM or root:wheel ownership, limits writes to them, permits non-elevated reads & audits every parent, owner, ACL & link. Existing policy, registrations, floors, provenance & keys are preserved. User-writable trust fails closed.

Verifiers accept valid fast-forwards automatically; shorter prefixes & forks fail. Approve-key/revoke-key advance local checkpoint within existing elevation. CI pins root in immutable protected config & automatically advances separately protected checkpoint state after chain verification. No normal sync ceremony exists.

Local doctor/build checks keep ownership findings as warnings until signed log has enrolled both Windows & Mac. Once both have appeared, revocation cannot restore enrollment-pending grace; revoked keys lose authority & zero active keys fail closed. Protected CI/release evidence requires completed enrollment.

Git transport is bounded & optional. Offline commands retain local commits; requests/revocations become visible to another machine after ordinary repository sharing. Offline verifiers use local committed log & checkpoint, so they learn revocations when updated log arrives. No fetched key gains authority without signed admission back to pinned root.

Commands run verifier non-elevated after installation. Cancellation/failure never retries native authentication or elevation. Tests enforce at most one of each per command. Helpers retain visible exception review & canonical payload signing. Existing exception audits remain in force. If every active private key is lost, root reset requires explicit administrator recovery.

## Acceptance checks

- Signed N-device chain, Windows RSA encodings & Mac P-256 interoperability.
- Unsigned add, forged root, malformed/domain-tampered events & revoked signers rejected.
- Root/issuer revocation preserves valid descendants; duplicate/re-added keys rejected.
- Fast-forward accepted; rollback/fork rejected against protected checkpoint.
- Exact committed blobs bound into scan/evidence; dirty worktree log cannot substitute authority.
- Independent product repositories use shared committed authority without per-product key copies.
- Policy merge preserves existing keys; same-host re-enrollment & root replacement refused.
- Protected owner/read ACL accepted; user-write, unsafe owner & reparse parents rejected.
- One native authentication + one elevation ceiling; cancellation causes no retry.
- CI checkpoint updates serialized & isolated from immutable config/code.

Automated native checks compile helpers & use ephemeral software keys. Adrian performs real enrollment/authentication; build output never asserts enrollment.

Mac helper must be packaged as a Developer ID signed app bundle with stable bundle ID `com.orthiclabs.rightkit.ownership-approval` & valid Team ID `6KLGD3LLKF` signature. `macos/build.sh <existing-node> <output.app>` uses shared signer without a provisioning profile or restricted entitlements. Before Keychain access, helper validates its bundle ID & designated Developer ID signature. Private Secure Enclave key representations & public metadata live in the explicit default unlocked user Keychain, protected by a legacy ACL trusting current signed helper; reads/adds disable Keychain interaction. Secure Enclave signing retains `.userPresence` & `.privateKeyUsage` controls. Native preflight validates identity & Keychain availability without creating a key, installing trust or authenticating. Native storage self-test uses only a UUID sentinel, verifies current-helper access & unrelated-executable denial, then removes & verifies sentinel cleanup. Optional same-identity updated-helper probe verifies ACL continuity without interaction.
