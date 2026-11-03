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

LicenseView carries `mode`, `status`, native `lifecycle`, `tier`, `features`, `kind`, `stage`, nullable Unix-millisecond `validUntil`, `confirmationDueAt`, `staleSince`, nullable `licenseIdSuffix`, `busy`, `unresolved`, `revisionState` & `message`. Unknown fields are discarded. Malformed required fields reject with fixed unknown error. Unknown message codes use fixed unknown text. Invalid change events stop their subscription & report fixed error through optional `onError`.

Helpers `isLicensed`, `isPro`, `tierAtLeast`, `hasFeature`, `hasAllFeatures`, `hasAnyFeature` & `satisfiesRequirement` consume LicenseView. Access hints fail closed during busy/unresolved states. Dates never extend grace or grant access. Native code must authorize protected operations when invoked.

Breaking migration: remove `StoredLicense`, storage/device/host/fetch config, `load`, `refreshWithOutcome`, token readers & `classifyLifecycle`. Move credential migration into native code. Replace nested entitlement reads with `view.tier` & `view.features`; remove local grace/clock arguments. Licence key & trial email are transient command inputs only.
