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

Methods: `status()`, `activate(key)`, `refresh()`, `deactivate()`, `requestTrial(email)` & `onChange(callback, onError?)`. `refresh()` returns `{view, outcome}`; trial returns `{accepted, message}`. Mutations reject with `LicenseClientError`, containing safe `code`, optional rejection `reason` & fixed English `message`. View `message` is a MessageCode; use `licenseMessage(view.message)` for display.

LicenseView carries `mode`, `status`, native `lifecycle`, `tier`, `features`, `kind`, `stage`, nullable Unix-millisecond `validUntil`, `confirmationDueAt`, `staleSince`, nullable `licenseIdSuffix`, `busy`, `unresolved`, `revisionState` & `message`. Unknown fields are discarded. Malformed required fields reject with fixed unknown error. Unknown message codes use fixed unknown text. Invalid change events stop their subscription & report fixed error through optional `onError`.

Helpers `isLicensed`, `isPro`, `tierAtLeast`, `hasFeature`, `hasAllFeatures`, `hasAnyFeature` & `satisfiesRequirement` consume LicenseView. Access hints fail closed during busy/unresolved states. Dates never extend grace or grant access. Native code must authorize protected operations when invoked.

Breaking migration: remove `StoredLicense`, storage/device/host/fetch config, `load`, `refreshWithOutcome`, token readers & `classifyLifecycle`. Move credential migration into native code. Replace nested entitlement reads with `view.tier` & `view.features`; remove local grace/clock arguments. Licence key & trial email are transient command inputs only.

Browser QA uses separate export:

```ts
import { createLicenseMock } from '@rightkit/license/testing';
const { client } = createLicenseMock();
```

Mock implements frozen native commands in memory & carries `RIGHTKIT_LICENSE_TESTING_DO_NOT_SHIP`. Testing export resolves only under `development`; default resolution is blocked. Vite development & Vitest can use mock. TypeScript projects using testing export need `customConditions: ["development"]` for declaration resolution.

Release owner must force actual production resolution, including `NODE_ENV=production`, & exclude custom `development` conditions: inherited `NODE_ENV=development` can make even `vite build --mode production` resolve mock. Keep final frontend artifact sentinel gate after bundling/minification. Export conditions & final-artifact gate serve separate checks.
