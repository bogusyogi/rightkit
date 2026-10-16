# Sync key recovery (rightkit-sync-keys)

Status: proposed, 2026-10-16, revised after Council CV-1 (round 1). The verdict was UNRESOLVED:
three seats could not run, and the forcing rule applies. The adjudicator ruled REVISE on the merits,
with 14 verified findings (F-1 to F-14), all applied below. Charter: significant, D2, `best_shape`,
critical rigor. Owner: rightkit-sync-keys. Issues: rightkit#118, #144, #145.

Intent: this answers the lead-relayed request (2026-10-16) for "ONE recommended design with its
reasons" on the sync key model. It follows the earlier relayed question of whether a passkey could
replace the recovery-phrase-only design. These are relays from the lead's chat, not Adrian's typed
words (F-11).

## Decision

**First release: two kinds of recipient, devices and one generated recovery key, under a signed
membership log.** Platform-keychain recovery and passkeys come later, as opt-in recipients added
under the same rules.

1. **Device recipients.** Each device keeps the data key, its own recipient key and its signing key
   in the OS credential store. A new device joins by approval from an existing one: the 6-digit code
   transfer (#145, built).
2. **The recovery key** (required, the floor). It is 128 random bits, shown once as Crockford Base32
   with a check symbol (26 symbols in groups of four). It goes on a printable Recovery Kit or into a
   password manager, and the user may keep several copies. Sync does not start until the user proves
   they saved it by typing two groups back. It is never a user-chosen phrase.
3. **Later, opt-in:** a platform-keychain recipient (iCloud Keychain on Apple, Block Store on Android)
   and a passkey PRF recipient. Each comes with an explicit trust disclosure, and neither is ever on
   by default (F-3).

**The passkey answer** (the question that started this): yes, a passkey can hold the sync key
through the WebAuthn PRF extension, which derives a key from the passkey. It should not *replace*
the recovery key. Native PRF is confirmed only on Apple (iOS 18 / macOS 15). It reached Windows Hello
in the February 2026 Windows 11 update, it is unconfirmed for native Android apps, and PRF output is
tied to one passkey provider. It is a later convenience (F-13). Build details are in the appendix.

## Why this design

- **Generated entropy, not a password.** The server is a Cloudflare Worker with D1, and the design
  adds no escrow or HSM infrastructure (this is the infrastructure assumption, F-1). Without a
  guess-limited HSM, a wrapped key under a user-chosen password or PIN is exposed to offline guessing
  by anyone who copies the database. Its strength is then the user's password strength.
  Signal SVR3, Apple's escrow HSMs, WhatsApp's key vault and Google's Titan vault accept low-entropy
  secrets only because of such hardware. A generated 128-bit key removes the dependence on password
  quality, and it is what provider-blind systems converge on: the 1Password Secret Key, Signal's
  backup key, Apple ADP's recovery key and Proton's phrase.
- **Devices do the daily work.** A new device is approved by an existing device, as in Signal
  linking, Apple trusted devices and Bitwarden device approval. The recovery key is only for the
  "every device lost" case.
- **Recipients are public keys.** Rotation (after a stolen device) seals the new data key to every
  remaining recipient, including the recovery key's public half, without asking the user for the
  kit. Other devices pick up the rotated key themselves.
- **Rejected alternatives** (F-2, F-4):
  - **Device-only.** It has no independent recovery once every enrolled device is gone.
  - **Passkey-only.** Platform coverage is incomplete, and the key is tied to one provider.
  - **Password or PIN, even with Argon2id.** Offline guessing, as above; this was the previous plan.
  - **Phrase only.** Every new device would need the phrase.
  - **Recovery contact (as in Apple ADP).** It adds identity, authorization, contact-loss and
    revocation flows. A contact device that holds the full data key can read the data, and N-of-M
    shares would need their own design. Not for the first release.

## Key hierarchy

- `DK_v`: the account data key, version v (exists: 32 bytes, `KeyId` from HKDF; zone keys come from
  it). Old versions are kept so old records still decrypt.
- Each recipient has an X25519 key (sealing) and an Ed25519 key (signing membership changes).
  - **Device**: both keys random, stored in the credential store.
  - **Recovery**: both keys come from `HKDF-SHA256(rk128, "rk-keyring/1 recovery x25519" |
    "... ed25519")`. Nothing is stored. Generating a new kit makes a new secret, because the data key
    can't reconstruct an old kit (F-12).
  - **Later platform recipient**: the platform keychain holds that recipient's *private keys*, never
    `DK` itself, so every method stays a recipient (F-3).
- `seal(DK_v -> R)`: an ephemeral X25519 key, then HKDF-SHA256, then XChaCha20-Poly1305, the same
  primitives as transfer.rs. The associated data (AAD) is new: `rk-keyring/1`, app, account,
  recipient id, kind, `KeyId`, v and the membership-log head it was sealed under (F-14).

## Membership log (replaces the MAC'd manifest: F-7)

The old design accepted any manifest whose MAC verified under a key version the client held. A
server could replay a pre-removal manifest, and a revoked device that still holds old keys could
forge one. This version replaces it with an append-only log, the pattern used by Apple's iCloud
Keychain circle of trust, where each device signs the circle's membership, and by the MLS group
protocol (RFC 9420), where each membership change is a signed commit.

- **Entries**: `{seq, parent_hash, op: add | remove | rotate | replace_recovery, subject recipient
  (id, kind, label, x25519_pub, ed25519_pub), key_version, key_id, author_id, signature}`. The
  signature is Ed25519 over a canonical encoding: a length-prefixed field list with a domain label.
  `seq` is separate from the key version.
- **Authorization**: the first entry (genesis) is self-signed by the first device and co-signed by
  the recovery key. After that, an entry is valid only if its author is a member at `parent_hash` and
  is not removed. `add` needs the author's signature plus the new member's signature over the same
  entry, which proves the new member holds its keys. A removed recipient's key never signs again.
- **Local trust anchor**: every device stores the head it last verified (`seq`, hash). It accepts
  only a chain that extends that head, so anything that doesn't extend it is a fork or a rollback.
  That is a hard error: the device refuses to sync and shows "your sync keyring was changed
  unexpectedly". It never just warns.
- **Rotation** seals `DK_{v+1}` only to the members of the device's *own* verified head after the
  removal. It never uses a membership list the server sends.
- **Residual risk, stated plainly**: a device with no local state can't detect a fork. That covers
  a fresh recovery on a new device, and an old device restored from a backup made before the
  removal. Such a device trusts the chain it is served after checking that it is internally valid
  and that the recovery key is a member. When it next meets any honest device's chain, a fork is
  detected and both refuse. This needs a server that forks the log *and* a removed member's keys.
  No log without an outside witness fully closes it. A future option is to publish log heads to a
  transparency service.

## Storage and transport (F-14)

- **A reserved `_keyring` zone** in `rightkit-sync-http/1`. It holds one record per log entry,
  `log/<seq>`, written with `if_server_unchanged` so two concurrent writers can't both extend the
  same `seq`; the loser rebases. It also holds one record per sealed key, `seal/<recipient>/<v>`.
  `HttpBackend` rejects arbitrary fields, so `KeyringBackend` carries each entry or sealed blob,
  serialized, in the `enc` field, with `updatedAt` and `deletedAt` as the protocol requires.
  `EncryptingBackend` is not used for this zone. Records are never deleted. Removal is a log entry.
- **Worker change (needed, F-10).** Tokens today identify the app and account only (`auth.ts`
  `Principal`), and revocation is account-wide (the erase marker in `guard.ts`). Add a device id
  claim, a per-device revocation list on the account's shard checked in admission, and an
  account-backend duty to stop issuing tokens for a removed device. This is access revocation from
  an honest server. Cryptographic exclusion comes from rotation, because a malicious server can
  ignore revocation.
- **Reuse and extension:**

| Owner | Use |
|---|---|
| `DataKey`, `KeyId`, zone keys | Reused as-is. |
| Transfer protocol (#145) | Reused as-is for new-device approval. Its X25519/HKDF/XChaCha primitives are shared; the keyring AAD is new. |
| `rightkit-credentials` | Reused for device secrets. The synchronizable item (`apple_helper.rs`) is kept for the later platform recipient. |
| `rightkit-sync` `HttpBackend` | Reused as the transport. |
| sync worker | Extended: device id claim and per-device revocation. Erase reused. |

## Flows

| Situation | What happens |
|---|---|
| First device | Creates `DK_1`, its keys and the recovery key, then writes the genesis entry and both seals. Shows the Recovery Kit and requires the two-group check before the first sync. |
| New device, another at hand | 6-digit approval. Then an `add` entry signed by both devices, and a seal for it. |
| All devices lost | Enter the recovery key, which opens `seal(DK -> recovery)`. The device checks the chain, then adds itself, signed by itself and the recovery key. The app offers a new kit. |
| Ordinary device loss or theft | From another device: a `remove` entry, rotation to `DK_{v+1}` sealed to the remaining members, and device revocation at the worker. The kit is kept (F-8). |
| Suspected compromise (kit seen, or a device lost unlocked with the kit) | A guided flow: remove the device, `replace_recovery` with a new kit (verified by the two-group check before completion), then rotate. The app says that any recipient kept can read future data. |
| Kit lost, devices fine | "Make a new recovery key": `replace_recovery`, then a new kit and the two-group check. |
| Everything lost | Unrecoverable, and setup says so. "Reset sync" creates a new key and log, and erases the server copy (exists). |

## Migration, concurrency, failure (F-6)

- **Versioning.** Keyring records start with a schema byte. A client that sees a newer schema stops
  syncing that account and says "update the app". It never writes. Clients without keyring support
  never touch `_keyring`.
- **Commit order.** Seals are written before the log entry that refers to them. A head whose seal is
  missing is treated as not yet committed, and the device retries. A crash between the two steps
  leaves an unreferenced seal, which is harmless.
- **No rollback promise** after a rotation: data written under `DK_{v+1}` needs it. Rolling back the
  app means keeping the keyring code, not disabling it.
- **Platform recipients (later)** each need primary-source evidence of their security properties and
  native backup and restore tests before they ship. That includes Block Store with end-to-end
  encryption unavailable, and a changed platform account.

## Acceptance cases (written now, run after the freeze: F-5)

1. Fresh-device recovery, including records encrypted under older key versions.
2. Remove-and-rotate leaves the removed device unable to open `DK_{v+1}`.
3. Kit replacement: the old kit fails afterwards, and the new kit must be verified.
4. Malformed, unsigned, wrongly signed or non-member entries are rejected.
5. A replayed pre-removal head is rejected by any device that has seen the removal.
6. Same-`seq` substitution and a forked chain are hard errors.
7. A write interrupted between seal and entry recovers.
8. An account-, app- or recipient-substituted seal fails its AAD check.
9. A malformed or mistyped recovery key fails before activation, through the check symbol.

Outcome measures, local and user-exportable only (no mandatory telemetry): setup completed with the
kit verified, and recovery success or failure reason.

## Diagnostics and support (F-9)

- **Exportable diagnostics**: log head, `seq`, key versions held, recipient ids and kinds, and the
  last error code. Never kit text, secrets, seals' plaintext or decrypted content.
- **Support scripts** cover each error: enrollment failed, keyring changed unexpectedly, interrupted
  rotation, lost kit, and everything lost. Support cannot recover data, and the scripts say so.
- **Server counters are hints only.** Client-verified log state is the authority.

## Changes in rightkit-sync-keys

1. A `recovery` module: `RecoveryKey::{generate, parse, display, recipient_keys}`. The Argon2id
   phrase wrap is dropped from the plan.
2. A `keyring` module: `Recipient`, `seal_to` and `open_as` (with the new AAD), and `Log` with
   `{genesis, append, verify_extends(local_head), members_at(head)}`, using canonical encoding and
   Ed25519 (a new dependency: `ed25519-dalek`, already allowlisted for several apps).
3. `KeyManager`: device signing key and trusted-head storage, `enroll_first_device` (returns the
   kit once), `approve_device`, `recover`, `remove_and_rotate`, `replace_recovery_key`.
4. `KeyringBackend` over any `Backend` (the `_keyring` zone, entries in `enc`, conditional writes).
5. Worker: a device id claim and a per-device revocation check (sync-worker); the account backend
   stops tokens for removed devices.

## Appendix: passkey PRF evidence (later phase)

As of 2026-10-16:
- **Apple**: native `ASAuthorizationPublicKeyCredentialPRF*` APIs from iOS 18.0 and macOS 15.0 (Apple
  docs). Cross-device bugs before iOS 18.4 (third-party report).
- **Windows Hello**: from Windows 11 24H2 26100.7840 or 25H2 26200.7840 (February 2026), according
  to third-party reports. Native `webauthn.dll` takes PRF salts, and Microsoft documents no minimum
  build.
- **Android**: Credential Manager documentation does not mention PRF for native apps (unverified).

A native probe on each OS comes before any PRF recipient ships.
