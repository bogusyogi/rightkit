## Unreleased

- `createProBridgeClient({ invoke, commands? })` (`status`, `setSyncEnabled`, `suppressConsumption`, `beforeDeactivate`, `afterActivate(view)`, `notifyAfterActivation`) and `PRO_BRIDGE_COMMANDS` (defaults `pro_bridge_status`, `pro_bridge_set_sync_enabled`, `pro_bridge_suppress_consumption`, `pro_bridge_after_activation`): the renderer half of Apple-device Pro continuity. The rule: suppress consumption before a deactivation, clear it only after an activation that answers `active`; suppress is best effort. `dist/proBridge.*` join the published files.

## 0.3.2

Additive; defaults and wire behaviour unchanged. `dist/lifecycle.js` and `dist/lifecycle.d.ts` join the published files.

- Lifecycle (lifted from the live HeardRight app, product parts as host configuration, exported from the package root): `snapshotFromLicenseView(view, messages?)`, `shouldRefreshLicenseFromServer`, `millisecondsUntilLicenseCheck`, `expiredTrialAcknowledgementKey`, `dialogForLicenseLifecycle(state, { appName, paidTier, spelling? })`, `isExpiredTrialNoticeAcknowledged` / `acknowledgeExpiredTrialNotice(state, { storageKey, storage? })` (the legacy full-id key still counts) and `createLicenseLifecycleMonitor({ client, afterChange?, changeEventNames?, fallbackMs?, messages? })` returning `{ get, subscribe, refresh, start }` in place of module singletons. Same order and rules as the app: retry queued releases (errors ignored), status, a server refresh only when explicit and the state needs it, `afterChange`, then subscribers; one check in flight; timer `min(max(delay, 1), fallbackMs)`; triggers focus, visibility, the client's `onChange` and the host's event names.
- Message overrides: `createLicenseClient({ messages })`, `createAccountClient({ messages })` and `licenseMessage(code, messages?)` take the host's own wording by message code. `LICENSE_MESSAGES_EN_US` is the US spelling. New `accountDeletionMessage(view, messages?)` (`ACCOUNT_DELETION_MESSAGES`) words how an account deletion ended.

# 0.3.1 (unpublished)

Additive. Existing exports and wire behaviour are unchanged. Not compiled; each call needs the native command it names.

- Removed (breaking, Adrian, 2026-11-03: no dummy data, no test helpers): the `@rightkit/license/testing` export (`createLicenseMock`, `RIGHTKIT_LICENSE_TESTING_DO_NOT_SHIP`) and `dist/testing.*`. Browser QA runs against the real installed build and the real native licence commands. The release scans that refuse the sentinel stay.
- Issue #1130: `createLicenseClient` gains `requestLink()` (`rightkit_license_link_request`, answers `issued` with `code` and `expiresAt`, `notLeased` or `notActiveLeaseHolder`) and `redeemLink(code, deviceLabel?)` (`rightkit_license_link_redeem`, answers the new `LicenseView`). Type `LinkRequestView`.
- Issue #1036: `retryPendingReleases()` (`rightkit_license_retry_pending_releases`) answers `{ attempted, settled, kept, acknowledgeFailed }` (`PendingReleaseDrain`), the counts of one native drain of the app's queued releases. Rejects as `unsupported` when the host has no queue.
- Issue #1032 (reconciled with `rightkit-tauri-shell`): `AccountErrorCode` also carries `forbidden` and `unsupported`, the host refusals of a window the policy does not trust and of an app with no account service. Commands and DTO casing verified against `host::account` and `AccountSessionClient`.
- Issue #1032: `createAccountClient({ invoke })` over the nine `rightkit_account_*` commands (`status`, `refresh`, `request_sign_in`, `complete_sign_in`, `sign_out`, `list_devices`, `rename_device`, `remove_device`, `delete`): `status()`, `refresh()`, `requestSignIn(email)`, `completeSignIn(code)`, `signOut()`, `listDevices()`, `renameDevice(device, label)`, `removeDevice(device)`, `deleteAccount({ confirmed: true })`. Views `AccountStatusView`, `AccountDeviceView`, `AccountDeletionView`; failures are `AccountClientError` with a closed `code` and `reason` and fixed text (`ACCOUNT_MESSAGES`). No token reaches the renderer. The shell must register `ServiceId::ACCOUNT` and the commands.

# 0.3.0

- Breaking: use native invoke/listen client & safe LicenseView DTOs; credentials, persistence & lifecycle evaluation stay native.
- Remove renderer fetch, storage config, plugin-http dependency & local grace classification.
- Add fixed errors, response/event validation & separate testing-only native command mock.
- Restrict testing export to development resolution; release builds must force production environment & exclude development conditions.

# Changelog

## 0.2.1 — unpublished staging

- Repairs license refresh and lifecycle handling.

Versions 0.1.5 to 0.1.8 below are reconstructed from git history (`git log -p -- packages/license`).
npm publication dates were not checked; the dates are commit dates.

## 0.2.0

Additive. Existing exports and `refresh()`'s return type are unchanged. The one behaviour change is
how `refresh()` treats the cached signed token when the server omits one (see "Behaviour changes").
Pairs with `rightkit-license` 0.2.0.

### Added

- `refreshWithOutcome(): Promise<{ outcome, license }>` on the client. `outcome` is a typed union:
  - `tokenReplaced`: the server minted a new `signedToken`; it replaced the cached one.
  - `tokenKept` (`reason`: `server_omitted_token` | `server_returned_same_token`, `validUntil`): the
    server said active but sent no token, or sent the one we hold, and the cached token is still within
    its own `exp`.
  - `tokenDropped` (`reason`: `no_cached_token` | `cached_token_lapsed`): the server said active but no
    usable cached token remains. `signedToken` is removed from the record; `status` stays `active`.
  - `stale` (`staleSince`, `error`): the server could not be reached (offline, 5xx, unstructured error,
    malformed body). The cached entitlement stands.
  - `revoked` (`message`): the server authoritatively said the license is no longer active.
  - `noCredential`: no `activationToken`, so no request was sent and nothing was written.
  `rejected` exists only natively: JS never verifies the signed token, so it cannot reject one.
- `refresh()` still returns `StoredLicense | null`, but the record now says what happened.
  `StoredLicense.lastRefreshOutcome` carries the outcome kind and `StoredLicense.staleSince` (ISO) is set
  while the server is unreachable. `staleSince` keeps the FIRST failure of an offline episode and is
  cleared by the next refresh that reaches the server.
- Lifecycle classification mirroring `rightkit-license`'s `Lifecycle`: type `Lifecycle` (`unlicensed`,
  `migration_required`, `trial`, `expired_trial`, `active`, `grace`, `expired`, `lifetime_provisional`,
  `verification_required`, `lifetime`, `clock_repair_required`, `revoked`, `invalid`),
  `classifyLifecycle(lic, { graceDays, now, requireSignedToken, clockRolledBack })`,
  `lifecycleGrantsAccess`, `lifecycleNeedsOnlineRefresh`. For an active record it agrees with
  `isLicensed` (tested over a matrix). It is a UI hint; the native value is the authority.
  JS checks only the kind/stage pairing. The date-order checks need the signed `iat` and stay native.
- `LicenseConfig.now?: () => number`, an injectable clock (epoch ms) for tests.

### Behaviour changes

- When the server answers `active` but omits `signedToken` (or returns blank), 0.1.8 and earlier kept
  the cached token unconditionally. 0.2.0 keeps it only while its own `exp` (read from the unverified
  payload, a UI-side decision) is in the future; otherwise it is removed (`tokenDropped`). A cached token
  that cannot be decoded counts as lapsed. If your backend has no signing key and never returns a token,
  every refresh now reports `tokenDropped` / `no_cached_token`; the record is otherwise unchanged.
- A failed refresh now writes `staleSince` and `lastRefreshOutcome` to storage. Before, it wrote nothing.
  Hosts that map `StoredLicense` to a stricter schema (for example a Rust struct) should ignore or
  carry these two optional fields.
- Revocation is unchanged: it sets `status: 'invalid'` and keeps the signed token in storage. Native
  authorization decides from the token, not from `status`.

### Not changed

- `isLicensed`, the readers, `satisfiesRequirement`, `activate`, `deactivate` and the HTTP error
  handling of 0.1.8 are untouched.

## 0.1.8 (2026-10-05, `03bbeeef`)

Authoritative server rejections are no longer mistaken for outages, and server messages reach the user.
The change is in the shared `post()` error path, so it affects `activate()` and `refresh()`.

Before (0.1.7 and earlier), any non-2xx response threw `Error('http <status>')` with `httpStatus` set:

- `activate()` stored `status: 'failed'` with the message `Activation server is not live yet.` for HTTP 404
  and `Could not reach the activation server.` for every other status.
- `refresh()` treated every non-2xx as "offline" and kept the cached license.

From 0.1.8, for a non-2xx response the body is parsed as JSON (a parse failure yields no body):

- **Structured rejection.** If the status is 4xx (400 to 499) AND the body has `ok === false` AND
  `typeof body.status === 'string'` AND `body.status !== 'active'`, the body is returned as the
  response, exactly as if it were a 200. Consequences:
  - `activate()` stores `status: 'invalid'` with `message: body.message`, or `License key was not
    accepted.` when the body has none. Examples: 404 `not_found` ("License not found or inactive"),
    409 `seat_limit` ("All device seats are in use").
  - `refresh()` sees a non-active status and sets the stored license to `status: 'invalid'` with the
    server's message (or `License is no longer active`): an authoritative revocation.
- **Everything else** throws `Error(message)` with `httpStatus` set, where `message` is `body.message`
  when it is a string, else `Activation request failed (HTTP <status>).` Consequences:
  - `activate()` stores `status: 'failed'` with that message. The special 404 text `Activation server is
    not live yet.` is gone: an unstructured 404 now reads `Activation request failed (HTTP 404).` A thrown
    network error (no `httpStatus`) still reads `Could not reach the activation server.`
  - `refresh()` still treats it as an outage and keeps the cached license. This covers 5xx, proxy HTML
    error pages, malformed bodies, and 4xx responses that are not a structured `ok:false` rejection.
- A 4xx body that claims `status: 'active'` is never trusted as a success; it is treated as an error.

Tests added: structured 404 and 409 on activate, an `active` body on a 404, a proxy HTML 404, and
`refresh()` distinguishing HTTP 404 (revoked) from HTTP 503 (outage).

## 0.1.7 (2026-08-28, `343654bf`)

- Toolchain alignment only: `packageManager` `pnpm@11.18.0` to `pnpm@11.24.0`. No source change.

## 0.1.6 (2026-07-17, `27b96e6e`)

Shared signed license lifecycle contracts.

- Types `LicenseKind` (`trial` | `lifetime` | `term`) and `EntitlementStage` (`time_bound` |
  `provisional` | `final`). `StoredLicense` gains `licenseKind`, `entitlementStage`, `licenseIssuedAt` and
  `confirmationDueAt` (server-owned UX hints; native authorization trusts `signedToken`).
- `activate()` stores them; `refresh()` carries them forward (`??` fallback to the cached value;
  `confirmationDueAt` and `validUntil` honour an explicit `null`).
- `isLicensed`: trial and provisional-lifetime licenses get no offline grace and lapse at exactly
  `validUntil`. The comparison changed from `now <= until + grace` to `now < until + grace`, so the
  boundary instant itself is now expired for every kind. An unparseable `validUntil` still does not lapse
  a graced license, but does lapse a trial or provisional one.

## 0.1.5 (2026-07-12, `d7599c16`; npm metadata fixed 2026-07-14, `5c2fbbb8`)

- First release from this repository: the shared license client (online token exchange against the
  RightApps backend, per-kind offline grace, entitlement readers).
- 2026-07-14: removed the duplicated Rust payload from the npm package (`files` no longer lists `rust`);
  the `rightkit-license` crate is the sole native policy source. A package-contract test now enforces
  that no Rust files ship in the npm package.

