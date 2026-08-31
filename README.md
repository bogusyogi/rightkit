# RightKit

Shared, platform-neutral primitives for every Right Suite app.

| Package | Runtime | Responsibility |
|---|---|---|
| [`@rightkit/ax`](packages/ax) | Node CLI | Agent-experience, plugin, conformance, behavioral, and adversarial gates (`right-ax`) |
| [`@rightkit/build-control-core`](packages/build-control-core) | Node | Managed-agent/strict profiles, request normalization, external namespaces & unmanaged-output scanning |
| [`@rightkit/build-control-broker`](packages/build-control-broker) | Node service | Digest-pinned tool execution, external cache environment, quotas, reservations, leases & typed receipts |
| [`@rightkit/build-control-clients`](packages/build-control-clients) | Node CLI | Explicit RightKit entrypoint, Rust/Cargo agent shims & request-only frontend registry |
| [`@rightkit/build-control-policy`](packages/build-control-policy) | Node hook | Rust/Cargo agent command policy & wrapper/compound-command gap detection |
| [`@rightkit/build-control-staging`](packages/build-control-staging) | Node | External transactional staging & sync-back contract |
| [`@rightkit/git`](packages/git) | Node CLI | Public-repository GitHub Actions workflow templates and visibility gate (`right-git`) |
| [`@rightkit/legal`](packages/legal) | TypeScript | Legal manifest schema 2, assent receipt, and release-contract primitives |
| [`@rightkit/legal-ui`](packages/legal-ui) | TypeScript + React | Shared desktop legal acceptance gate |
| [`@rightkit/license`](packages/license) | TypeScript | License network, storage, and UX client |
| [`@rightkit/logs`](packages/logs) | TypeScript + Rust payload | Durable local JSONL logging and privacy-safe diagnostics bundles |
| [`@rightkit/platform-ui`](packages/platform-ui) | TypeScript + React | Platform detection, shortcut rendering, capabilities, and headless window controls |
| [`@rightkit/qa`](packages/qa) | Node CLI | Cross-platform desktop QA gates |
| [`@rightkit/release`](packages/release) | Node CLI | Native-host signed release, hardening, explicit public GitHub or private R2 + RightApps publication, and add-on adoption |
| [`@rightkit/tauri`](packages/tauri) | TypeScript | Typed Tauri host adapter for license and device commands |
| [`@rightkit/updates`](packages/updates) | TypeScript | Runtime update policy helper |

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

Native enforcement is supplied only by the separately published
`rightkit-license` crate on crates.io. Tauri apps use the exact registry version
from `packages/release/rightkit-versions.json`; Git, path, sibling-checkout, and
vendored-copy dependencies are forbidden.

In that manifest, `cargo` is the published version registry used to enforce app
dependency pins. `stagedCargo` records canonical crate versions awaiting
publication and never changes consumer enforcement. After a crate is published,
promote its staged version into `cargo` and remove its `stagedCargo` entry before
updating app pins.

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

<!-- cortex:docs:start -->
## Repository truth docs
- [Product overview](docs/product.md) — what this is and does (generated, code-grounded)
- [Architecture](docs/architecture.md) — components, flows, interfaces (generated, code-grounded)
<!-- cortex:docs:end -->

## Managed-agent Rust builds

Configured coding agents use RightKit as a request-only broker client:

```bash
rightkit cargo test
rightkit cargo build --release
rightkit rustc --version
rightkit rustdoc --version
```

Direct `cargo`, `rustc`, `rustdoc`, & `rustup run …` are intentionally denied in
managed-agent contexts. Do not substitute an absolute toolchain binary, alter
`PATH` or Rust/Cargo environment, set a target directory, add `.cargo`
configuration or aliases/wrappers, or write local `target`/`.cache` output.
`pnpm` & `npm` remain usable; their child Cargo invocation must inherit RightKit
shim precedence.

Each request is capability-classified, durably admitted, process-owned, & closed
with one terminal receipt. Disconnect detaches observer; STATUS, ATTACH, & CANCEL
operate by request ID. Failed/cancelled namespaces stay warm under retention.
Independent maintenance generation-CAS moves eligible terminal namespaces to
managed trash, then deletes them in bounded resumable batches.

### Operations

On macOS, managed roots live below configured external storage (normally
`/Volumes/D/rightkit-build-control`); the broker endpoint is a short Unix socket
from `RIGHTKIT_BUILD_BROKER_SOCKET`. On Windows, the same external root holds
`rightkit-build-control`; broker & sccache use native Windows IPC. In both cases,
global Cargo home, toolchains, compiler cache, workspace targets, build state,
temporary state, & receipts stay below that root.

Runtime installation publishes a self-contained version directory, verifies
socket plus functional `cargo --version` health, atomically switches its current
pointer, & rolls back if post-switch health fails. To investigate any non-zero or
`125` result, inspect broker service health first, then its terminal receipt &
reported violation. Restore broker/external-volume availability or remove only
receipt-identified unmanaged output; never retry through a direct Rust command.

Cleanup is maintenance-owned, never in admission or terminal hot paths. It uses
bounded inventory, durable leases/intents, generation checks, & resumable trash
deletion. sccache is disabled unless broker ownership is proven. Windows BUILD
fails closed until native Job Object containment exists.

### Rust targets for new apps

macOS bootstrap registers native Apple Silicon, Intel macOS, physical iOS, &
Apple Silicon iOS Simulator targets. New apps use those targets directly:

```bash
rightkit rustup target add aarch64-apple-ios
rightkit rustup target add aarch64-apple-ios-sim
rightkit cargo check --target aarch64-apple-ios
rightkit cargo check --target aarch64-apple-ios-sim
```

`target add` is globally exclusive & writes into same Rustup home that owns
broker-registered Cargo/rustc. If a future app needs another target, add it to
host bootstrap `targetTriples`, add exact bootstrap/admission tests, then use
guarded bootstrap cutover. Never hand-edit live `config.json`; bootstrap
regenerates it.

### Publication boundary

Build-control workspace components are private implementation packages, not
consumer dependencies; they are never independently published. Their delivery is
one exact commit on `bogusyogi/claude` `main`, with no npm-publication claim.

See [Managed Build Control](docs/RIGHTKIT-THIN-BUILD-CONTROL.md) for health, receipts,
recovery, cleanup, GC, & platform operations.

Cross-host Rust validation uses content-addressed Mac snapshots, managed Windows
replicas, & each host's local broker. See [Dual-Host Rust Routing](docs/RIGHTKIT-DUAL-HOST-BUILD-ROUTING.md).
