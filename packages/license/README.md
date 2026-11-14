# @rightkit/license

Version 0.3.0 replaces renderer network & storage with native commands. Native code owns credentials, lifecycle, clocks, persistence & authorization. Renderer receives validated display views.

```ts
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { createLicenseClient, licenseMessage } from '@rightkit/license';

const license = createLicenseClient({ invoke, listen });
const view = await license.status();
const stop = await license.onChange(view => render(view));
// On component cleanup:
stop();
```

Methods: `status()`, `activate(key)`, `refresh()`, `deactivate()`, `requestLink()`, `redeemLink(code, deviceLabel?)`, `retryPendingReleases()`, `requestTrial(email)` & `onChange(callback, onError?)`. `refresh()` returns `{view, outcome}`; trial returns `{accepted, message}`. Mutations reject with `LicenseClientError`, containing safe `code`, optional rejection `reason` & fixed English `message`. View `message` is a MessageCode; use `licenseMessage(view.message)` for display.

`createAccountClient({ invoke })` is the passwordless account session over the native `rightkit_account_*` commands: `status()`, `refresh()`, `requestSignIn(email)`, `completeSignIn(code)`, `signOut()`, `listDevices()`, `renameDevice(device, label)`, `removeDevice(device)` and `deleteAccount({ confirmed: true })` (irreversible; call it only after the person confirmed). It rejects with `AccountClientError` (closed `code` and `reason`, fixed text). The session token stays native.

`createProBridgeClient({ invoke, commands? })` is the renderer half of Apple-device Pro continuity: `status()`, `setSyncEnabled(on)`, `suppressConsumption(on)` (best effort), `beforeDeactivate()` (suppress first) and `afterActivate(view)` (clear only when the activation answers `active`). Command names default to `pro_bridge_*` and are options.

Lifecycle: `createLicenseLifecycleMonitor({ client, afterChange?, changeEventNames?, fallbackMs? })` follows the licence for the UI (`get`, `subscribe`, `refresh({ online? })`, `start()`) and `snapshotFromLicenseView`, `dialogForLicenseLifecycle(state, { appName, paidTier })` and the expired-trial notice helpers (`isExpiredTrialNoticeAcknowledged`, `acknowledgeExpiredTrialNotice`, each with `{ storageKey }`) turn it into display state. Display only. Wording: pass `messages` (by message code) to `createLicenseClient`, `createAccountClient` and `licenseMessage`; `LICENSE_MESSAGES_EN_US` is US spelling; `accountDeletionMessage(view)` words a deletion outcome.

App layer: `createAppLicense({ invoke, client, proBridge?, updatedEvent, commands?, messages?, windowsLinkWording })` is the licence layer of an app that answers its own commands next to the shell's (`refresh_entitlements`, `open_pricing_page`, `license_windows_link_request` and `_redeem`, `license_update_headers`, ...; override any through `commands`): credential-store state (`storage.get`, `storage.subscribe`), the lifecycle `monitor`, `activateLicense`, `removeLicense`, `requestTrialKey`, link codes, `updateHeaders`. `createAppAccountSession({ invoke, commands?, messages?, invalidInput?, deletionMessages? })` is the account session over the app's own `account_session_*` commands (`sessionStatus`, `refresh`, `beginMagicLink`, `complete`, `signOut`, `devices`, `renameDevice`, `removeDevice`, `deleteAccount`); its rejections are `Error`s worded by `accountCommandError`. Both take the host's sentences as options; no product name is in the package.

LicenseView carries `mode`, `status`, native `lifecycle`, `tier`, `features`, `kind`, `stage`, nullable Unix-millisecond `validUntil`, `confirmationDueAt`, `staleSince`, nullable `licenseIdSuffix`, `busy`, `unresolved`, `revisionState` & `message`. Unknown fields are discarded. Malformed required fields reject with fixed unknown error. Unknown message codes use fixed unknown text. Invalid change events stop their subscription & report fixed error through optional `onError`.

Helpers `isLicensed`, `isPro`, `tierAtLeast`, `hasFeature`, `hasAllFeatures`, `hasAnyFeature` & `satisfiesRequirement` consume LicenseView. Access hints fail closed during busy/unresolved states. Dates never extend grace or grant access. Native code must authorize protected operations when invoked.

Breaking migration: remove `StoredLicense`, storage/device/host/fetch config, `load`, `refreshWithOutcome`, token readers & `classifyLifecycle`. Move credential migration into native code. Replace nested entitlement reads with `view.tier` & `view.features`; remove local grace/clock arguments. Licence key & trial email are transient command inputs only.
