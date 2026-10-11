# @rightkit/license

The license client for Right Suite apps. It exchanges a license key for an activation token (and a signed token when the backend returns one) against the RightApps backend (`https://<host>/v1/licenses/*`), stores the result through an app-supplied storage port, and applies per-kind offline grace.

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

Added in 0.2.0: `classifyLifecycle`, `lifecycleGrantsAccess`, `lifecycleNeedsOnlineRefresh`; types `Lifecycle`, `ClassifyOptions`, `RefreshOutcome`, `RefreshOutcomeKind`, `RefreshResult`.

`createLicenseClient` returns `{ activate(key), refresh(), refreshWithOutcome(), deactivate(), load() }`.

## Refresh semantics

`refreshWithOutcome()` returns `{ outcome, license }`. `refresh()` returns just the record, which carries `lastRefreshOutcome` and, while the server is unreachable, `staleSince`.

| Outcome | When | Record |
|---|---|---|
| `tokenReplaced` | active, new `signedToken` | token replaced |
| `tokenKept` | active, token omitted or identical, cached token's `exp` still in the future | unchanged |
| `tokenDropped` | active, token omitted or identical, none cached or cached one expired/undecodable | `signedToken` removed, `status` stays `active` |
| `stale` | no network, 5xx, unstructured error, malformed body | unchanged except `staleSince` (first failure of the episode) and `lastRefreshOutcome`, set to `stale` |
| `revoked` | structured 4xx rejection, or a non-`active` status | `status: 'invalid'` |
| `noCredential` | no `activationToken` | nothing sent, nothing written |

The lifecycle classification (`classifyLifecycle`) is a UI hint derived from the stored record. The Rust `Lifecycle` in `rightkit-license` is the authority.

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

- Inside a Tauri webview, requests use `@tauri-apps/plugin-http` to avoid a CORS preflight that the brand hosts do not answer. The app must install that plugin and its capability. Without it, the client falls back to the global `fetch`, and the source comment says the brand hosts do not answer that CORS preflight (`src/index.ts:155-162`).
- `settings-ui` peers this package with no upper bound (`>=0.1.0`).
