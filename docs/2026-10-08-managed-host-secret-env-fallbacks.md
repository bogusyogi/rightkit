# Managed-host secret environment fallbacks — 2026-10-08

Report only; secret-loading code remains unchanged. Static scan covered RightKit
`packages/` & `crates/` source for `process.env`, environment aliases/copies,
computed environment lookups & `std::env::var` / `var_os` reads with names
containing `TOKEN`, `KEY`, `SECRET` or `PASSWORD`. No runtime environment,
credential store or secret values were read.

## Proposed managed-host boundary

When managed-host marker is present, refuse ambient secret environment inputs
before selecting credentials or starting subprocesses; return typed
`MANAGED_HOST_ENV_SECRET_REFUSED` naming variable only. Use provisioned protected
credential source instead. Keep unmanaged-host behavior unchanged.

Use existing `brokerManagedHost` detection at
`packages/release/cargo-contract.mjs:31`: installed managed runtime, broker socket
marker (`RIGHTKIT_BUILD_BROKER_SOCKET`) or managed Cargo shim. Socket marker alone
misses non-login shells (`packages/release/build-release.mjs:205`). Refusal must
also apply when broker is unavailable; missing service must not permit fallback.

## Findings & proposed fixes

| Location | Environment input & current behavior | Proposed fix when managed-host marker is present |
|---|---|---|
| `packages/release/release-token.mjs:22` | `RIGHTAPPS_RELEASE_TOKEN` directly overrides protected macOS Keychain / Windows token file. | Refuse direct env token; load provisioned Keychain service `rightkit-rightapps-release-token` or protected Windows token file. |
| `packages/release/release-token.mjs:25`, `:27` | `RIGHTAPPS_RELEASE_TOKEN_FILE` redirects token loading to arbitrary env-selected file. | Refuse env file override; require protected host configuration for token-file location. |
| `packages/release/model-promote.mjs:107` | `RIGHTKIT_RUNTIME_ARTIFACT_PRIVATE_KEY` supplies inline PEM before file lookup. | Refuse inline env key; resolve provisioned protected signing key through host configuration. |
| `packages/release/model-promote.mjs:109` | `RIGHTKIT_RUNTIME_ARTIFACT_PRIVATE_KEY_FILE` selects secret file when explicit file is absent. | Refuse env fallback; accept only independently authorized protected key configuration / explicit file subject to protection checks. |
| `packages/release/sign-updater.mjs:17`, `:18`, `:27` | Copies `process.env`; inherited `TAURI_SIGNING_PRIVATE_KEY` bypasses Windows protected-file lookup. Inherited `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` reaches signing child. | Refuse caller env key/password; resolve protected Windows signer or macOS signing agent first, then supply signer-owned child inputs. |
| `packages/release/macos-signing-session.mjs:175`, `:177`, `:178`, `:181` | Defaults to `process.env`; inherited updater key/password can bypass Keychain or GUI-agent selection in local session. | Refuse inherited `TAURI_SIGNING_PRIVATE_KEY` / `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`; use provisioned signing agent or Keychain lookup. |
| `packages/release/addon-command.mjs:94` | Requires ambient `CLOUDFLARE_API_TOKEN` before R2 upload; this is required env auth rather than fallback after store lookup. | Refuse ambient token; retrieve credential from provisioned protected source & supply it only to upload child. |
| `packages/release/prune-r2.mjs:12` | Reads ambient `CLOUDFLARE_API_TOKEN` for Cloudflare requests, with no protected-source loader. | Refuse ambient token; load protected credential inside prune operation. |

## Related secret selectors & forwarding

| Location | Finding | Proposed fix when managed-host marker is present |
|---|---|---|
| `packages/release/notary-auth.mjs:13`, `:14` | `APPLE_API_KEY_PATH` selects private `.p8` file; `APPLE_API_KEY` is public key ID. Together with issuer, automatic selection overrides default Keychain profile. Explicit profile already takes precedence. | Refuse env-selected API auth; use provisioned Keychain profile or protected host signer configuration. Preserve public key IDs as metadata. |
| `packages/release/apple-profile-renew.mjs:17`, `:19` | Defaults to ambient env & requires API key ID/path for profile renewal before loading private file. | Refuse env auth selectors; resolve provisioned protected API signer configuration. |
| `packages/release/pub-upload.mjs:35`, `:80`, `packages/release/upload-large.mjs:35`, `:80` | Copy ambient environment for Wrangler. They delete `CLOUDFLARE_API_KEY` & `CF_API_KEY`, but forward inherited `CLOUDFLARE_API_TOKEN` without reading it explicitly. | Refuse caller token before spawn; inject protected-source token into minimal child environment. Retain existing legacy-key deletion. |

## Crate result & exclusions

- `crates/rightkit-mcp/examples/example_server.rs:115` reads `EXAMPLE_TOKEN` for
  demonstration HTTP server auth. Example-only, not established managed-host
  production integration. If deployed on managed host, refuse env token & inject
  credential retrieved by host from `rightkit-credentials`; do not silently start
  unauthenticated HTTP server after refusal.
- No production Rust literal secret-name reads were found. Dynamic Rust reads
  examined included `rightkit-credentials/src/store.rs:80`; its guard names control
  credential-store access rather than supply secrets.
- Rust secret-store test fixtures (`crates/rightkit-credentials/tests/e2e.rs:170`,
  `:171`) read child test keychain/password selectors; excluded from production
  findings.
- `RIGHTKIT_RUNTIME_ARTIFACT_KEY_ID`
  (`packages/release/model-promote.mjs:211`) & `RIGHT_RELEASE_CACHE_KEY`
  (`packages/release/cache-command.mjs:67`) are identifiers, not secrets.
- `packages/release/rightkit-signing-agent.mjs:187`, `:188` inject updater
  credentials obtained from Keychain into freshly constructed child env;
  `:204` removes them. This is protected-store output, not ambient fallback.
- `packages/release/publication-gate.mjs:96`, `:150` inject
  `RIGHTKIT_NPM_TOKEN` obtained from Keychain; these are assignments, not env
  fallback reads. Cloudflare legacy-key deletion & model-upload secret deletion
  likewise are not credential reads.
- Generic configuration expansion / preflight lookups do not establish specific
  secret fallbacks without consumer configuration; this report identifies source
  paths, not claims of installed integration.

Acceptance for future remediation: managed marker + each listed ambient input
must produce refusal without credential use, subprocess launch or secret-bearing
diagnostics; managed host with protected credential source must succeed;
unmanaged compatibility must remain intact.

## Pending protected loaders

Refusal is applied only where a protected source exists and remains usable
(release token, runtime-artifact signing key, updater signing, macOS signing
session, notarization API auth).

Status: implemented. `packages/release/protected-credentials.mjs` loads the
protected values before any environment fallback is considered. Managed hosts
refuse the named variable (`MANAGED_HOST_ENV_SECRET_REFUSED`) before any store
read. Unmanaged hosts read the store first and use the variable only when the
store entry is absent. A missing entry fails with a typed
`PROTECTED_SECRET_MISSING` error that prints the store command. Values are never
logged. Storage follows `release-token.mjs`: macOS Keychain generic passwords
(account `$USER`) and, on Windows, protected token files under
`%APPDATA%\RightKit\`. Windows does not use Credential Manager; the existing
token-file pattern was reused rather than adding a second store.

- Cloudflare R2 / API token: `CLOUDFLARE_API_TOKEN` in `packages/release/addon-command.mjs`
  (`publishR2Addon`), `packages/release/prune-r2.mjs` (`pruneReleaseObjects`), and
  `packages/release/upload-large.mjs`. The upload path injects the token into the
  Wrangler child environment only. `r2-verified-upload.mjs` reads no token, so it is
  unchanged. Keychain service `rightkit-cloudflare-api-token`; Windows file
  `cloudflare-api-token.token`. Provisioning per `docs/rules/cloudflare-access.md`.
- Apple App Store Connect API key auth in `packages/release/apple-profile-renew.mjs`
  (`appleProfileAuth`). No Keychain entry and no token file. The key ID and issuer
  are identifiers, not secrets: they come from the optional release config block
  `appStoreConnect: { keyId, issuer, keyPath }` first, then from `APPLE_API_KEY`
  and `APPLE_API_ISSUER` on every host. The `.p8` is read from the file at the
  config `keyPath` first, then from the existing `APPLE_API_KEY_PATH` used by
  notarization. Managed hosts refuse any Apple environment value that carries
  inline PEM key material (`MANAGED_HOST_ENV_SECRET_REFUSED`); a file path in the
  environment is allowed.

Store commands: the Cloudflare token is the only protected value. Run once per
host; it prompts for the value and never takes it on the command line.

macOS:

```
security add-generic-password -a "$USER" -s rightkit-cloudflare-api-token -U -w
```

Windows (PowerShell; the file is created under `%APPDATA%\RightKit`):

```
powershell -NoProfile -Command "$s = Read-Host -AsSecureString 'Cloudflare API token'; $d = Join-Path $env:APPDATA 'RightKit'; New-Item -ItemType Directory -Force -Path $d | Out-Null; [IO.File]::WriteAllText((Join-Path $d 'cloudflare-api-token.token'), [Runtime.InteropServices.Marshal]::PtrToStringBSTR([Runtime.InteropServices.Marshal]::SecureStringToBSTR($s)))"
```

Apple needs no store commands. Set `appStoreConnect` in the release config, or set
`APPLE_API_KEY`, `APPLE_API_ISSUER`, and `APPLE_API_KEY_PATH` in the environment.
