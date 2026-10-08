# @rightkit/license

The license client for Right Suite apps. It exchanges a license key for a signed token against the RightApps backend (`https://<host>/v1/licenses/*`), stores the result through an app-supplied storage port, and applies per-kind offline grace.

| Item | Value |
|---|---|
| Where it runs | App webview UI (client) talking to the RightApps backend. Offline token verification runs natively. |
| Rust pair | `rightkit-license` (native Ed25519 entitlement verification and enforcement) |
| Version | see INDEX.md |
| License | MIT OR Apache-2.0 |

## Entry points

| Import | Contents |
|---|---|
| `@rightkit/license` | `createLicenseClient(config)`, `isLicensed`, `isPro`, `tier`, `hasFeature`, `hasAllFeatures`, `hasAnyFeature`, `tierAtLeast`, `satisfiesRequirement`; types `Entitlements`, `LicenseTier`, `LicenseRequirement`, `LicenseStatus`, `LicenseKind`, `EntitlementStage`, `StoredLicense`, `LicenseStorage`, `LicenseConfig` |

`createLicenseClient` returns `{ activate(key), refresh(), deactivate(), load() }`.

## Peer dependencies

| Peer | Range | Required |
|---|---|---|
| `@tauri-apps/plugin-http` | `^2` | Optional (`peerDependenciesMeta`). Needed inside a Tauri webview. |

## Example

```ts
import { createLicenseClient, isPro } from "@rightkit/license";

const license = createLicenseClient({
  app: "coderight",
  host: "coderight.cc",
  appVersion: "1.0.0",
  storage, // { get(): Promise<StoredLicense | null>; set(lic): Promise<void> }
  deviceId: () => deviceId,
});

const lic = await license.activate("AD-XXXX");
const pro = isPro(lic);
```

## Known issues

- Inside a Tauri webview, requests use `@tauri-apps/plugin-http` to avoid a CORS preflight that the brand hosts do not answer. The app must install that plugin and its capability. Without it, requests fail.
- `settings-ui` peers this package with no upper bound (`>=0.1.0`).
