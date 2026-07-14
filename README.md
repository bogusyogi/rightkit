# RightKit

Shared, platform-neutral primitives for every Right Suite app.

| Package | Runtime | Responsibility |
|---|---|---|
| [`@rightkit/license`](packages/license) | TypeScript + Rust payload | License network, storage, UX client, and signed entitlement enforcement payload |
| [`@rightkit/logs`](packages/logs) | TypeScript + Rust payload | Durable local JSONL logging and privacy-safe diagnostics bundles |
| [`@rightkit/release`](packages/release) | Node CLI | Signed release, hardening, patch/update routing, R2 upload, and RightApps registration |
| [`@rightkit/updates`](packages/updates) | TypeScript | Runtime update policy helper |
| [`rightkit-process`](crates/rightkit-process) | Rust | Ownership-safe child lifecycle, restart, and health primitives |

This is the only source home for shared RightKit packages and crates in the
Claude repo. Apps consume the published npm packages; they do not depend on
this checkout, Git URLs, or sibling paths.

## @rightkit/license

The **one** license client for every Right Suite app. Online token-exchange against the unified RightApps backend, with a locally-cached entitlement + per-kind offline grace so the app tolerates brief offline use while the **server stays authoritative** (revocation + tier/feature changes land at the next refresh).

Replaces the five diverged per-app license clients. Platform-agnostic: the host injects `storage` (keyring-backed in Tauri, in-memory in tests) and `deviceId`, so this module has zero Tauri/DOM coupling and is fully unit-testable.

> **Enforcement note.** This JS client is the *validation + network + UX* layer. `isLicensed()` /
> `hasFeature()` are UI hints, **not** a tamper-proof gate — anyone can edit JS. For on-device Pro
> features that must resist a casual bypass, the gate has to live in Rust, verifying a server-signed
> entitlement token. That design (server signs · Rust verifies + enforces · JS carries) is spec'd in
> **[ARCHITECTURE.md](ARCHITECTURE.md)**.

---

## The model (read this first)

- **Server-authoritative.** The backend (`rightapps-api`) is the source of truth for whether a license is active, what tier/features it grants, and whether it's been revoked. The app does **not** self-validate a signed blob.
- **Online token-exchange.** `activate(key)` exchanges a raw purchase key for an **activation token** (the raw key is then dropped). Every later call uses the token, never the key.
- **Per-kind offline grace.** A **perpetual** license (`validUntil === null`, e.g. a lifetime/creator license) **NEVER lapses offline** — a lifetime buyer is never bricked by our server being down. A **time-bound** license (subscription, `validUntil` set) keeps working offline until `validUntil + graceDays`, then lapses.
- **Revocation propagates on `refresh()`.** Call `refresh()` on launch + on a cadence. A refund/dispute sets the license `status = 'revoked'` server-side; the next refresh returns `invalid` and the app drops Pro. (Superwhisper-style: uninstall/reinstall doesn't restore Pro, because the server decides.)

---

## Install

Consumed as a public package from npm:

```bash
pnpm add "@rightkit/license@^0.1.5"
```

Do not install this SDK from `git+https://github.com/adrdsouza/rightkit.git` or a Git tag. App repos depend on the registry package version in `package.json`; lockfiles pin the resolved package integrity.

The npm tarball also carries the matching Rust crate under `rust/`. Tauri apps
copy and commit that directory as `src-tauri/vendor/rightkit-license`, then use
`rightkit-license = { path = "vendor/rightkit-license" }`. Cargo must never fetch
RightKit from Git or require a sibling checkout.

---

## Tauri apps — required native wiring (CORS)

The brand hosts don't answer the webview's CORS preflight (`OPTIONS /v1/licenses/* → 404`), so a plain in-app `fetch` is blocked before it leaves the app. **The SDK handles the JS half for you** — when `__TAURI_INTERNALS__` is present it auto-routes every request through the **Tauri HTTP plugin** (Rust-side, no preflight); you do NOT inject `fetchImpl`. But you must wire the parts the SDK can't reach into:

1. **npm:** `pnpm add "@tauri-apps/plugin-http@^2"` (so the SDK's lazy import resolves in your bundle)
2. **Cargo** (`src-tauri/Cargo.toml`): `tauri-plugin-http = "2"`
3. **Register** in the builder (`lib.rs` / `app_lib.rs`): `.plugin(tauri_plugin_http::init())`
4. **Capability** (`src-tauri/capabilities/default.json` → `permissions`):
   ```json
   { "identifier": "http:default", "allow": [{ "url": "https://<brand-host>/*" }] }
   ```

Belt-and-suspenders: also add `https://<brand-host>` to the CSP `connect-src` in `tauri.conf.json`. Browser/QA builds (no Tauri runtime) fall back to the global `fetch` automatically.

---

## Quick start

```ts
import { createLicenseClient, isLicensed, hasFeature, type LicenseStorage } from "@rightkit/license";

// 1. Host-provided persistence. In Tauri, back this with the OS keyring; in tests, a Map.
const storage: LicenseStorage = {
  async get()  { /* return StoredLicense | null */ },
  async set(l) { /* persist l, or clear when l === null */ },
};

// 2. Stable per-install device id (you derive it from keyring/localStorage).
const deviceId = () => getOrCreateDeviceId();

// 3. One client per app.
const license = createLicenseClient({
  app: "coderight",        // appKey — sent as `app`, matched server-side
  host: "coderight.cc",    // brand host -> https://coderight.cc/v1/licenses/*
  appVersion: "0.1.0",
  storage,
  deviceId,
  graceDays: 7,            // optional; offline grace for TIME-BOUND licenses only
});

// activate / refresh / deactivate / load
const lic = await license.activate(userTypedKey);  // exchanges key -> token, persists
await license.refresh();                            // re-validate (call on launch)
await license.deactivate();                         // free this device's seat + clear

// gate features (pure, no I/O — safe every render)
if (isLicensed(lic) && hasFeature(lic, "wake_word")) { /* unlock */ }
```

### Reference implementation

The cleanest end-to-end consumer (keyring storage adapter, boundary mapper, updater-gating, feature reads) is **CodeRight**: [`apps/coderight-tauri/src/state/updateLicense.ts`](https://github.com/adrdsouza/coderight). MailRight (`src/lib/license.ts`) and ViewRight (`src/state/updateLicense.ts`) are the same pattern with an updates-only gate (no tier/features). Copy one of those.

### The boundary-mapper pattern

Each app keeps its own existing license type (`StoredUpdateLicense`) so its updater + UI don't churn. Wrap the SDK with two tiny mappers — `toApp(SdkLicense)` / `fromApp(...)` — that translate `validUntil ↔ updatesUntil`, `entitlements.features ↔ features`, etc. The SDK owns network + token + storage; the app owns presentation. See any of the three reference files.

---

## API

| Export | Signature | Notes |
|---|---|---|
| `createLicenseClient(cfg)` | `→ { activate, refresh, deactivate, load }` | one per app |
| `activate(licenseKey)` | `→ Promise<StoredLicense>` | exchanges key→token, persists. `status:'active'` is the **only** success signal; non-active → `invalid`; HTTP 404 → `failed` ("not live"); other HTTP/network error → `failed`. |
| `refresh()` | `→ Promise<StoredLicense \| null>` | re-validates. **Network failure KEEPS the cached license** (grace decides offline, not the server). Revoked/expired → `invalid` + persisted. |
| `deactivate()` | `→ Promise<void>` | best-effort seat release + clears storage. Idempotent. |
| `load()` | `→ Promise<StoredLicense \| null>` | reads cached license from storage. |
| `isLicensed(lic, graceDays?, now?)` | `→ boolean` | `active` AND (perpetual OR within `validUntil + grace`). Perpetual never lapses offline. |
| `tier(lic)` | `→ string \| null` | `lic.entitlements.tier` |
| `hasFeature(lic, f)` | `→ boolean` | `lic.entitlements.features.includes(f)` |

`LicenseConfig`: `{ app, host, appVersion, storage, deviceId, graceDays?, fetchImpl? }`.
`StoredLicense`: `{ status, deviceId, key?, activationToken?, licenseId?, validUntil?, entitlements?, message?, activatedAt?, lastRefreshedAt? }`.
`Entitlements`: `{ v: number, tier: string | null, features: string[] }` — **versioned**; `v` bumps on a breaking change.

---

## Backend contract

The client talks to `https://<host>/v1/licenses/*`, which the brand host forwards to `rightapps-api`.

| Method · Path | Body | Response |
|---|---|---|
| `POST /v1/licenses/activate` | `{ app, deviceId, licenseKey, version }` | `{ ok, status, activationToken, licenseId, seats, validUntil, entitlements, message }` |
| `POST /v1/licenses/refresh` | `{ app, activationToken }` | `{ ok, status, validUntil, entitlements }` |
| `POST /v1/licenses/deactivate` | `{ app, activationToken }` | `{ ok }` |
| `POST /v1/admin/licenses/{id}/revoke` · `/restore` | `{ reason? }` (admin auth) | kill switch — sets `status='revoked'`/restores |

`status` vocabulary: `active` (grant), `invalid` (revoked/expired/unknown — drop Pro), plus client-only `none`/`failed`. **`status === 'active'` is the only thing that grants.**

**Hosts that forward `/v1/` today:** `coderight.cc`, `mailright.cc`, `viewright.cc`, `heardright.app`. ScrapeRight has no host configured yet.

---

## Testing

`fetch` is resolved at **call time** (not client-creation time), so a test that spies on the global `fetch` after the module loads is intercepted normally:

```ts
vi.spyOn(globalThis, "fetch").mockResolvedValue({ ok: true, status: 200, json: async () => ({ status: "active", activationToken: "tok" }) } as Response);
const lic = await client.activate("KEY-123");
expect(lic.status).toBe("active");
```

Inject `fetchImpl` only if you need a non-global fetch. The SDK's own suite (`src/index.test.ts`) covers activate/refresh/deactivate + the grace logic.
