# Suite trust roots

Three values decide who a Right Suite app trusts: the key that signs **licence tokens**, the key(s) that
sign **updates**, and the **host** each app's licence client talks to. RightKit owns all three in one file,
`contracts/right-suite-api.v1.json`, and `scripts/gen-suite-api.mjs` projects them to every language. An
app declares which suite app it is and carries none of them.

| Value | Contract field | Rust (`rightkit_license::suite_api`) | TypeScript | Swift |
|---|---|---|---|---|
| Licence signer key (Ed25519) | `licensePublicKeyHex` | `LICENSE_PUBLIC_KEY` | `RIGHT_SUITE_LICENSE_PUBLIC_KEY_HEX` | `RightSuiteAPI.licensePublicKeyHex`, `.licensePublicKeyBase64url` |
| Updater keys (minisign) | `updaterPublicKeys[{kid, publicKey, notAfter?}]` | `updater_keys()`, `select_updater_key` | `updaterKeys()`, `selectUpdaterKey` (`@rightkit/app-update/suite-api`) | `RightSuiteAPI.updaterPublicKeys` |
| Key releases are signed with | `updaterSigningKeyId` | `UPDATER_SIGNING_KEY` | `updaterSigningKeyId()` | `RightSuiteAPI.updaterSigningKeyId` |
| Per-app registry | `apps.<app>.{licenseHost?, artifactHosts?}` | `registered_apps()`, `app_license_host(app)` | `RIGHT_SUITE_APP_KEYS`, `RIGHT_SUITE_APP_LICENSE_HOSTS` | `RightSuiteAPI.appKeys`, `.licenseHosts` |
| Licence client routes | `clientPaths` | `LICENSE_ACTIVATE_PATH` ... `ENTITLEMENT_PATH` | `RIGHT_SUITE_CLIENT_PATHS` | — |

The registry lists the nine suite apps (the mandatory set's `defaultApps` less Ember, which has its own
updater key). An app that has not declared a `licenseHost` is `NoLicenseHost`: no host is ever guessed
(2026-10-17, "no silent default host"). Today CutRight (build-time `VITE_RIGHTAPPS_HOST`), Pulse (a keychain
item) and PostRight (an unverified host, C3-03) have not declared one; Adrian declares them in the contract.
GenRight declares `heardright.app`, which is what its code uses today (rightkit#272 tracks its own domain);
changing it is one line.

An app builds its clients from the contract:

```rust
let config = LicenseClientConfig::suite("heardright", env!("CARGO_PKG_VERSION"), store, license_dir)?;
let update = TauriUpdateTransport::suite(app_handle, "heardright", UPDATE_TIMEOUT)?; // feed + every accepted key
```

## The inventory this replaces (checked out at the HEADs of 2026-10-24)

**Licence key, pasted as source** (the line the literal starts on; 11 in 6 apps. The audit counted 10 and missed the Worker):

| App | File:line | Form |
|---|---|---|
| CodeRight | `engine/bins/coderight/src/license.rs:48` | `[u8; 32]` |
| CodeRight | `apps/coderight-tauri/src-tauri/src/license_host.rs:64` | `[u8; 32]` |
| CodeRight | `apps/coderight-ios/CodeRight/License/LicensePublicKey.swift:11` | base64url |
| GenRight | `crates/genright-engine/src/entitlement.rs:42` | `[u8; 32]` |
| HeardRight | `tauri-app-next/src-tauri/src/host_services.rs:50` | `[u8; 32]` |
| HeardRight | `ios-native/Shared/LicenseEntitlement.swift:81` | bytes |
| HeardRight | `cloudflare-workers/model-serve/worker.js:62` | bytes (a server copy) |
| MailRight | `src-tauri/src/pro_gate.rs:41` | `[u8; 32]` |
| ScrapeRight | `crates/protocol/src/license.rs:10` | `[u8; 32]` |
| ScrapeRight | `ios-native/Shared/LicenseEntitlement.swift:16` | bytes |
| ViewRight | `src-tauri/src/license_host.rs:62` | `[u8; 32]` |

Not pasted but also a source of the key: Pulse reads it from the user's keychain (`hub/src-tauri/src/license.rs:34,98`, so a user can plant
their own signer key), CutRight takes hex from a build environment variable (`apps/cutright/src-tauri/src/licence.rs:59-60`), PostRight has
none (`LICENSE_PUBLIC_KEY: Option<[u8; 32]> = None`, `src-tauri/src/lib.rs:87`, which is why its licence step cannot be passed). Test
fixtures that embed the key are deliberately skipped by the check (MailRight `test-vectors/mailright-license-v2-production.json:5`,
HeardRight `scripts/test-account-session-contract.mjs:93`, RightKit's own `rightkit-models` "wrong key" fixture).

**Updater key, copied into `tauri.conf.json`** (`plugins.updater.pubkey`; 7 identical copies, PostRight has a placeholder):
CodeRight `apps/coderight-tauri/src-tauri/tauri.conf.json:40`, GenRight `src-tauri/tauri.conf.json:46`, ScrapeRight `:45`,
MailRight `:84`, ViewRight `:42`, HeardRight `tauri-app-next/src-tauri/tauri.conf.json:24`, CutRight `apps/cutright/src-tauri/tauri.conf.json:50`;
PostRight `:35` is `PLACEHOLDER-REPLACE-WITH-SUITE-UPDATER-PUBKEY`. Three apps also parse that conf by hand to build their update config:
CodeRight `update_host.rs:70`, MailRight `shell_plugin.rs:83`, ViewRight `license_host.rs:149`.
The Tauri updater plugin cannot start without a `pubkey` string in its config, so the copy stays until the plugin can be configured without
it. What changed is that it can only be a verified copy: `suite-updater-key` (preflight) refuses any conf key that is not a contract key.

**Licence host, from five sources:** a constant (CodeRight `license_host.rs:41`, MailRight `pro_gate.rs:30`, ViewRight `license_host.rs:42`,
HeardRight `host_services.rs:45`, ScrapeRight `protocol/src/license.rs:18`, PostRight `lib.rs:83`), a TypeScript constant (GenRight
`src/lib/license.ts:21`), a keychain item (Pulse `license.rs:36,101`), a build variable (CutRight `licence.rs:78`).

**Engine-side Pro check, three ways:** `CredentialTokenSource::from_record_store` (CodeRight `license.rs:181`, ScrapeRight
`main_license.rs:26`, MailRight `pro_gate.rs:77`), a hand-written keychain reader (GenRight `entitlement.rs:67,170`), and none. All of them make
the engine a second reader of the owner's store, against `rightkit-credentials` README ("only the owner binary reads the OS store").

Re-run the findings on any checkout with `node scripts/preflight.mjs --repo <app> --worktree`: `suite-key-literal` (C3-04),
`suite-updater-key` (C3-14) and `engine-record-store` (C3-05). `ci/app-check-baseline.json` lists today's findings per app; each app's
migration removes its entries.

## Engine entitlement: one pattern

The owner hands the engine a `rightkit_credentials::handoff` snapshot of three values and the engine verifies it again. Details are in the
`engine_snapshot` module docs and the `rightkit-license` README; the contract of the pattern:

* Owner: `LicenseClient::engine_snapshot()` when the engine starts and after every change (`LicenseClient::invalidation()`);
  `LicenseClient::answer_engine_request(&request)` to answer an engine that asks (only the three names, snapshot mode).
* Engine: `EngineSnapshotCell::apply(snapshot)`, then `EngineEntitlements::suite_snapshot(app, &cell, license_dir)`.
* The engine has no input for a verdict; a failed owner read arrives as unreadable (never Free, never a stale token); a delivery replaces what
  the engine held and invalidates its cached verdict. `EngineEntitlementsConfig::with_revoked_ids` keeps an incident denylist
  (MailRight's `mailright_test_pro` id) working after the move, as does `NativeEngineEntitlements::with_revoked_ids` and
  `LifecycleContext::with_revoked_ids`.
* The anti-rollback ledger must be the file ledger (`<license dir>/clock.json`); the engine only reads it (`OwnerLedgerReader`). An owner that
  configured `with_credential_clock` keeps its ledger in the keychain, which an engine cannot read, so that app moves to the file ledger first.
  Two processes that both wrote the ledger would take turns claiming its mark and refuse each other inside the same second.
* `from_record_store` is deprecated. The owner's device binding (the raw id for a typed key, its hash for a lease) is the one value the engine
  takes on the owner's word, so the transport must authenticate the owner before `apply`.

## Updater key rotation

The updater plugin verifies a download with exactly one key. The suite lists every key an app accepts, and a release names the key that signed
it inside its minisign signature, so the shell asks with the configured key first and, when the signature names another accepted key that has not
ended, asks again with that key (`update::accepted_keys::check_across_keys`, `TauriUpdateTransport::suite` / `with_suite_keys`). A release signed
by an unlisted or ended key is refused before anything downloads, and a server that changes its answer between the two requests is refused, never
mixed. The release tooling uses the same list: for a suite app `publish-update` and the seal take the key and its id from the contract
(`updaterKeyId` defaults to `updaterSigningKeyId`, a declared public key must be a contract key), and `sign-updater --key-id` refuses a signature
made by another key. Ember is not a suite app and keeps declaring its own key.

### Runbook: rotate A (signing today) to B

1. **Generate B** on the signing host with `tauri signer generate`, store the private key in the host credential provider, never in a repository.
   Read B's key id from the public key (the generator derives and checks it).
2. **Add B to the contract** (`updaterPublicKeys`), keep `updaterSigningKeyId` = A, run `node scripts/gen-suite-api.mjs`, merge, publish
   `rightkit-license`, `rightkit-tauri-shell` and `@rightkit/release`.
3. **Ship a transitional release R1 per app**, built against those versions and **signed by A**. Every R1 install now accepts A and B.
4. **Hold R1 as the latest release** until enough installs have taken it (the release status route, `releases/status.json`, shows the latest
   versions; the decision is Adrian's). A client that never installs R1 cannot verify a B-signed release.
5. **Switch**: set `updaterSigningKeyId` = B, regenerate, publish, provision B's private key as the one `sign-updater` uses, sign R2 with B.
   `publish-update` now refuses anything A signs.
6. **Retire A**: set `notAfter` on A and ship a release (signed by B) that carries it; from that date an install of that release refuses A. Drop A
   from the list only when no supported version still needs it.

Limits, stated plainly. A client below R1 is stranded at the B switch and must reinstall: step 4 is the mitigation, not a cure. If A is
compromised the recovery release can only be delivered under A's trust, so B should be listed long before it is needed. `sign-updater` picks
its private key by credential name today, so A and B cannot yet coexist in one provider; `--key-id` makes the wrong key an error, a per-key
credential account is the host-side follow-up. `notAfter` is checked against the wall clock; an unreadable clock refuses a key that has an end
date but a rolled-back clock can still admit it before that date.

## Rollback stays removed; the fail-forward design (not built)

Decision #771 D1 stands: no `.previous` copy is kept or restored. Today a version that never acknowledges its boots
(`createBootProbation`, default 2 attempts) yields `reinstall_required` and the app tells the user to reinstall. The proposed fail-forward
replaces that guidance with an install from the feed, which is a new install, not a restore:

1. **Feed.** `latest.json` gains an optional `previous` block (the prior release's `version` and per-platform `signature`/`url`). The publish
   tooling already downloads and merges the existing manifest, so it can carry the last release forward one level. Clients that do not know the
   field ignore it.
2. **Probation.** On `reinstall_required` for version N (from M) the host calls `check_repair(N)` instead of showing guidance. The transport asks
   the feed with the same authorization as any check (a paid `update`-tier release still needs the licence) and the same key selection as above.
3. **Target.** The repair target is the feed's `previous` release when its version is not N; otherwise there is none and today's guidance
   stands. It is accepted although its version is lower, which the normal comparator refuses, so repair has its own comparator that is
   reachable only from a recorded probation failure.
4. **Bounds.** One repair per failed version (`repairAttempted: N` in the probation record); a repaired install is on probation itself and
   failing again ends in guidance; N is quarantined locally so the next check does not offer it again until a release newer than N appears.
5. **Why that is safe.** The target is signed by an accepted key; it is installed only after a local failure, never on the server's say-so; the
   attacker who controls the feed and can also crash N would gain an old *signed* release, so the feed may also publish a minimum repair
   version in the release status, which the client enforces.

Open questions for Adrian: whether `previous` in `latest.json` or per-version manifests; whether repair may run without a prompt; the minimum
repair version policy. Nothing in this PR depends on them.

## Rollout: what an app meets when it takes the release that carries these rules

`suite-key-literal` (C3-04), `suite-updater-key` (C3-14) and `engine-record-store` (C3-05) are rules of `packages/release/app-check.mjs`, which every gated command of `@rightkit/release` runs first (`rightkit-gate check`, build, test, sign). RightKit's own `ci/app-check-baseline.json` ratchets the findings it knows (rightkit 1, coderight 4, genright 1, heardright 3, mailright 2, postright 1, scraperight 3, viewright 1), but that file applies only when the module is loaded from a RightKit checkout. An app that runs the installed package reads its own `.rightkit/app-check-baseline.json`, where each entry carries an `issue`, an `added` date and a `sunset` at most 90 days later. So when an app moves to the release with these rules, every gated command refuses until the app has migrated to the contract or listed its findings with dated entries. The wave 1 and wave 2 port pull requests add those entries before they pin that release. cutright and pulse have no findings.

Repositories outside the nine apps are not in the baseline: `rightsites` pastes the licence key as a byte array in `packages/rightsuite-api-plugin/src/artifact-delivery/license-gate.ts`, `artifact-delivery/models.ts` and `rightsites-plugin/runtime-artifact-resolve.ts`, and would fail `suite-key-literal` the first time it runs `rightkit-gate check`.

## What this change does not do

Migrating the nine apps (wave 3), removing the Tauri conf copy, wiring `suite-api-literal` into the build's prebuild gates (it is a preflight
rule, ratcheted, until the apps migrate), the contract test against the server's published route inventory (it needs the `sellright` checkout),
Swift compilation of the generated file (no Linux Swift build), and the key rotation rehearsal on the Mac and the Dell.
