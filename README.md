# RightKit

RightKit is the shared library for Right Suite apps. It ships three things:

- Rust crates under [`crates/`](crates). The public ones are published to crates.io as `rightkit-*`.
- npm packages under [`packages/`](packages). The public ones are published as `@rightkit/*`.
- The Swift package [`RightKitSwift`](swift/RightKitSwift/README.md), consumed by git URL and exact tag.

Current versions and the full crate and package inventory are in [INDEX.md](INDEX.md). It is generated; do not edit it by hand. The table below covers only part of that inventory.

Which package is in which app, and how far each app has ported, is in [docs/PORT-MATRIX.md](docs/PORT-MATRIX.md) (generated with INDEX.md; the same data as a filterable page is [docs/port-matrix.html](docs/port-matrix.html)).

| Package | Runtime | Responsibility |
|---|---|---|
| [`@rightkit/ax`](packages/ax) | Node CLI | Agent-experience, plugin, conformance, behavioral, and adversarial gates (`right-ax`) |
| [`@rightkit/ci`](packages/ci) | Node CLI | Public-repository GitHub Actions workflow templates and visibility gate (`right-git`) |
| [`@rightkit/issues-tracker`](packages/issues-tracker) | Node CLI | Per-repository `issues.html` tracker generated from GitHub issues (`right-issues`) |
| [`@rightkit/legal`](packages/legal) | TypeScript | Legal manifest schema 2, assent receipt, and release-contract primitives |
| [`@rightkit/legal-ui`](packages/legal-ui) | TypeScript + React | Shared desktop legal acceptance gate |
| [`@rightkit/license`](packages/license) | TypeScript | License network, storage, and UX client |
| [`@rightkit/logs`](packages/logs) + [`rightkit-logs`](crates/rightkit-logs) | TypeScript, Rust | Durable local JSONL logging and privacy-safe diagnostics bundles |
| [`@rightkit/media-ui`](packages/media-ui) + [`rightkit-playback`](crates/rightkit-playback) | TypeScript + React, Rust | Brand-neutral media player, image viewer/editor, and FFmpeg playback-compat core |
| [`@rightkit/platform-ui`](packages/platform-ui) | TypeScript + React | Platform detection, shortcut rendering, capabilities, and headless window controls |
| [`@rightkit/pdf-ui`](packages/pdf-ui) + [`@rightkit/doc-render`](packages/doc-render) + [`rightkit-pdf`](crates/rightkit-pdf) | TypeScript + React, Rust | PDF viewing, editing, merge/extract & sanitized Markdown/HTML rendering |
| [`@rightkit/theme`](packages/theme), [`@rightkit/settings-ui`](packages/settings-ui) & [`@rightkit/onboarding`](packages/onboarding) | TypeScript + React | Theme tokens, settings sections & legal-first onboarding |
| [`@rightkit/qa`](packages/qa) | TypeScript (Node) | Shared QA evidence, ownership, browser helpers, and release-backdoor guards |
| [`@rightkit/release`](packages/release) | Node CLI | Native-host signed release, hardening, explicit GitHub/R2 distribution, and add-on adoption |
| [`@rightkit/tauri`](packages/tauri) | TypeScript | Typed Tauri host adapters for license, device, log, and diagnostics commands |
| [`@rightkit/app-update`](packages/app-update) | TypeScript | Runtime update policy helper |
| [`RightKitSwift`](swift/RightKitSwift) | Swift package | Apple-platform license, diagnostics, artifacts, purchases, sync, store, speech, and UI primitives (15 library products) |

This repository (`bogusyogi/rightkit`) is the only source home for shared RightKit crates, npm packages, and the Swift package. Apps consume the published crates and npm packages, and the Swift package by exact tag. Consumers must not depend on this checkout, a sibling path, or a Git URL for crates or npm packages; `right-release` checks refuse such dependencies.

## Versions and lifecycle

Each shared version moves through four states: implemented, staged, published, adopted. `docs/RIGHTKIT-SHARED-PACKAGES.md` in the workspace root has a copy of the definition below.

### Lifecycle: implemented, staged, published, adopted

Every shared RightKit version moves through four states, in this order. Paths below
are relative to `rightkit`. The record for the last three states is
`packages/release/rightkit-versions.json`.

| State | Meaning | Recorded in | Changed by |
|---|---|---|---|
| implemented | Source at this version in the tree. Not on any registry. | `version` in `crates/*/Cargo.toml` or `packages/*/package.json`; `swift/RightKitSwift/VERSION` | Source edit |
| staged | The next version the workspace intends to release. May be unpublished. Release checks accept an app pin at this version; pin drift is measured against `registry.*`, not against it. | `stagedCargo`, `stagedNpm`, `stagedSwift` | `node packages/release/rightkit-sync.mjs stage <npm\|cargo> <name> <version>`, which refuses a version below the current one |
| published | A registry serves the version. | `registry.cargo` and `registry.npm`, verified by `rightkit-sync.mjs`. Swift has no registry: the `swift` section records the tag after `right-release publish swift` pushes it. | `rightkit-sync.mjs` after a registry check, or `node scripts/ci/publish.mjs ledger` for a `publish.yml` receipt; the `swift` section by hand. |
| adopted | An app pins the version. | Top-level `cargo`, `npm` and `swift` (the adopted pins). Consumer manifests: `Cargo.toml` `=x.y.z`, `package.json`, `Package.swift` `exact:`. | Each app, by hand. No command bumps app pins. |

Rules:

- Staging adds the staged version to the pins release checks accept; it removes none.
- When a registry serves a version newer than the staged one, `rightkit-sync.mjs`
  raises the staged entry to it. After publication, staged equals published until a
  newer version is staged.
- A version replaced in a registry stays in `legacyCargo` or `legacyNpm`, so apps
  pinned to it still validate.
- Once every app has moved, set the top-level pin to the new version by hand. The
  staged entry may then be deleted. Sync does not re-add it unless the registry serves
  a newer version.
- Swift pins are always exact tags. There is no range or registry resolution.

## Public-repository build boundary

Only Membrane and Legion have an authorized native Windows unsigned-installer
development exception, declared in `.rightkit-local-development.json` & enforced
by managed client, broker & command policy. See `docs/rules/rightkit.md` in workspace root.
The default public policy below still owns signing, publication & qualification.

Public repositories set `"compile": "github-actions-only"` in `.rightgit.json`. Each task receives one binding:

- `local-static-only`: edit or inspect source/docs/config; run only formatting, lint, schema, manifest, workflow-drift, syntax, or doctor checks guaranteed not to compile, test, package, sign, qualify, install, or release.
- `github-build`: compile, link, test, execute built code, package, sign, notarize, staple, qualify, installed-smoke, finalize, or publish. Push source & run generated `right-git` workflow on target-native GitHub runner.

Wrong or missing public manifest policy fails validation. `right-release` also exits `125` outside GitHub Actions in such a repository for build/release/package entrypoints, signing or updater creation, Cargo target resolution, add-on and DMG builds, & Cargo/Swift publication. A local broker, cross-host shell, package-manager wrapper, direct binary, or dry run cannot change task binding. This binding is the current manifest policy, not the target: [docs/GATE-CONTRACT.md](docs/GATE-CONTRACT.md) (locked 2026-10-28) has every app compile locally through the gate with Class A first, and CI qualifies public apps.

RightKit is not such a repository. It is private (`bogusyogi/rightkit`). Its crate, package, and Swift builds compile locally through the shared gate (Mac and Dell through the RustRight broker, Linux direct; second priority after RustRight); CI qualifies each push and publishes automatically from git ([docs/GATE-CONTRACT.md](docs/GATE-CONTRACT.md)). The npm and crates.io packages stay public. See [docs/CI.md](docs/CI.md).

Fast Rust templates cancel superseded jobs (pull-request runs always; a push to the default branch cancels an older run that only just started or is failing, and waits for one that is nearly done, unless `.rightgit.json` sets `concurrency.mainPushes` to `"queue"` with a recorded `concurrency.reason`, as RightKit's does), check out full history for change-aware gates, share Cargo caches, run `sccache`, & set `cache-on-failure: true`. Keep LTO & `codegen-units=1` out of fast-build policy. Current coverage is RightKit, Legion, Membrane, Citadel, ScreenRight, & static `orthic-labs.github.io`; static repositories add no compile workflow until dynamic work exists. Policy automatically covers future public `Orthic-Labs` repos.

Private repositories use no GitHub Actions. They build, test, package, sign, qualify, & release locally on native hosts through RightKit.

## @rightkit/license

The **one** license client for every Right Suite app. Online token-exchange against the unified RightApps backend, with a locally-cached entitlement + per-kind offline grace so the app tolerates brief offline use while the **server stays authoritative** (revocation + tier/feature changes land at the next refresh).

Replaces the five diverged per-app license clients. Platform-agnostic: the host injects `storage` (keyring-backed in Tauri, in-memory in tests) and `deviceId`. The only Tauri touchpoint is the optional HTTP-plugin routing described under "Tauri apps" below, and the module stays unit-testable.

> **Enforcement note.** This JS client is the *validation + network + UX* layer. `isLicensed()` /
> `hasFeature()` are UI hints, **not** a tamper-proof gate — anyone can edit JS. For on-device Pro
> features that must resist a casual bypass, the gate has to live in Rust, verifying a server-signed
> entitlement token. That design (server signs · Rust verifies + enforces · JS carries) is spec'd in
> **[ARCHITECTURE.md](ARCHITECTURE.md)**.

---

## The model (read this first)

- **Server-authoritative.** The backend (`rightapps-api`) is the source of truth for whether a license is active, what tier/features it grants, and whether it's been revoked. The JS client does **not** verify the server-signed token (`signedToken`); it carries it to storage for the native `rightkit-license` crate to verify.
- **Online token-exchange.** `activate(key)` exchanges a raw purchase key for an **activation token** (the raw key is then dropped). Every later call uses the token, never the key.
- **Per-kind offline grace.** A **perpetual** license (`validUntil === null`, e.g. a lifetime/creator license) **NEVER lapses offline** — a lifetime buyer is never bricked by our server being down. A **time-bound** license (subscription, `validUntil` set) keeps working offline until `validUntil + graceDays` (default 7), then lapses. Trial and provisional-lifetime licenses get no grace: they lapse at `validUntil`.
- **Revocation propagates on `refresh()`.** Call `refresh()` on launch + on a cadence. A refund/dispute sets the license `status = 'revoked'` server-side; the next refresh returns `invalid` and the app drops Pro.

---

## Install

Consumed as a public package from npm:

```bash
pnpm add --save-exact "@rightkit/license@<version>"  # current version: see INDEX.md
```

Do not install this SDK from `git+https://github.com/bogusyogi/rightkit.git` or a Git tag. App repos depend on an exact registry package version in `package.json`; release checks reject ranges for the `@rightkit/*` packages recorded in `packages/release/rightkit-versions.json`. Lockfiles pin the resolved package integrity.

Native enforcement is supplied only by the separately published
`rightkit-license` crate on crates.io. Tauri apps pin an exact `=x.y.z`
registry version recorded in `packages/release/rightkit-versions.json`; Git, path,
sibling-checkout, and vendored-copy dependencies are forbidden.

In `packages/release/rightkit-versions.json`, the top-level `cargo` entries are the adopted pins. Release checks accept an exact pin that is in `cargo`, `stagedCargo` or `legacyCargo`. `stagedCargo` holds the next version, and `registry.cargo` holds what crates.io serves. See the lifecycle section above.

---

## Tauri apps — required native wiring (CORS)

The brand hosts don't answer the webview's CORS preflight (`OPTIONS /v1/licenses/* → 404`), so a plain in-app `fetch` is blocked before it leaves the app. **The SDK handles the JS half for you** — when `__TAURI_INTERNALS__` is present it auto-routes every request through the **Tauri HTTP plugin** (Rust-side, no preflight); you do NOT inject `fetchImpl`. But you must wire the parts the SDK can't reach into:

1. **npm:** `pnpm add "@tauri-apps/plugin-http@^2"` (so the SDK's lazy import resolves in your bundle)
2. **Cargo** (`src-tauri/Cargo.toml`): `tauri-plugin-http = "2"`
3. **Register** in the app's Tauri builder (CodeRight: `src-tauri/src/lib.rs`): `.plugin(tauri_plugin_http::init())`
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
});

// activate / refresh / deactivate / load
const lic = await license.activate(userTypedKey);  // exchanges key -> token, persists
await license.refresh();                            // re-validate (call on launch)
await license.deactivate();                         // free this device's seat + clear

// gate features (pure, no I/O — safe every render)
if (isLicensed(lic) && hasFeature(lic, "wake_word")) { /* unlock */ }
```

### Reference implementation

The cleanest end-to-end consumer (keyring storage adapter, boundary mapper, updater-gating, feature reads) is **CodeRight**: [`apps/coderight-tauri/src/state/updateLicense.ts`](https://github.com/bogusyogi/coderight). MailRight (`src/lib/license.ts`) and ViewRight (`src/state/updateLicense.ts`) are the same pattern; ViewRight's gate is updates-only (no tier/features). Copy one of those.

### The boundary-mapper pattern

Each app keeps its own existing license type (`StoredUpdateLicense`) so its updater + UI don't churn. Wrap the SDK with two tiny mappers — `toApp(SdkLicense)` / `fromApp(...)` — that translate `validUntil ↔ updatesUntil`, `entitlements.features ↔ features`, etc. The SDK owns network + token + storage; the app owns presentation. See any of the three reference files.

---

## API

| Export | Signature | Notes |
|---|---|---|
| `createLicenseClient(cfg)` | `→ { activate, refresh, refreshWithOutcome, deactivate, load }` | one per app |
| `activate(licenseKey)` | `→ Promise<StoredLicense>` | exchanges key→token, persists. `status:'active'` is the **only** success signal; non-active → `invalid`, including an HTTP 4xx whose JSON body is `{ ok: false, status: <non-active> }`; any other HTTP error (such as an unstructured 404) or a network error → `failed`. |
| `refresh()` | `→ Promise<StoredLicense \| null>` | re-validates. **Network failure KEEPS the cached license** (grace decides offline, not the server). Revoked/expired → `invalid` + persisted. |
| `refreshWithOutcome()` | `→ Promise<{ outcome, license }>` | same refresh, plus a typed `outcome`: `tokenReplaced`, `tokenKept`, `tokenDropped`, `stale`, `revoked`, or `noCredential`. |
| `deactivate()` | `→ Promise<void>` | best-effort seat release + clears storage. Idempotent. |
| `load()` | `→ Promise<StoredLicense \| null>` | reads cached license from storage. |
| `isLicensed(lic, graceDays?, now?)` | `→ boolean` | `active` AND (perpetual OR within `validUntil + grace`). `graceDays` defaults to 7. Perpetual never lapses offline; trial and provisional-lifetime licenses get no grace. |
| `tier(lic)` | `→ string \| null` | `lic.entitlements.tier` |
| `hasFeature(lic, f)` | `→ boolean` | `lic.entitlements.features.includes(f)` |

Also exported: `isPro`, `tierAtLeast`, `hasAllFeatures`, `hasAnyFeature`, `satisfiesRequirement`, `classifyLifecycle`, `lifecycleGrantsAccess`, `lifecycleNeedsOnlineRefresh`, and the types `LicenseTier`, `LicenseRequirement`, `LicenseStatus`, `LicenseKind`, `EntitlementStage`, `RefreshOutcomeKind`, `RefreshOutcome`, `RefreshResult`, `Lifecycle`, `ClassifyOptions`. The readers are UI hints; native code must verify `signedToken`.

`LicenseConfig`: `{ app, host, appVersion, storage, deviceId, graceDays?, fetchImpl?, now? }`. The client does not read `graceDays`; pass `graceDays` to `isLicensed` and the other readers.
`StoredLicense`: `{ status, deviceId, key?, activationToken?, licenseId?, validUntil?, entitlements?, message?, activatedAt?, lastRefreshedAt?, signedToken?, licenseKind?, entitlementStage?, licenseIssuedAt?, confirmationDueAt?, staleSince?, lastRefreshOutcome? }`.
`Entitlements`: `{ v: number, tier: string | null, features: string[] }` — **versioned**; `v` bumps on a breaking change.

---

## Backend contract

The client talks to `https://<host>/v1/licenses/*`, which the brand host forwards to `rightapps-api`.

| Method · Path | Body | Response fields the client reads |
|---|---|---|
| `POST /v1/licenses/activate` | `{ app, deviceId, licenseKey, version }` | `{ ok, status, activationToken, licenseId, validUntil, entitlements, message, signedToken, licenseKind, entitlementStage, licenseIssuedAt, confirmationDueAt }` |
| `POST /v1/licenses/refresh` | `{ app, activationToken, deviceId }` | `{ ok, status, validUntil, entitlements, message, signedToken, licenseKind, entitlementStage, licenseIssuedAt, confirmationDueAt }` |
| `POST /v1/licenses/deactivate` | `{ app, activationToken }` | none; a failure is ignored |

`status` vocabulary: `active` (grant), `invalid` (revoked/expired/unknown — drop Pro), plus client-only `none`/`failed`. **`status === 'active'` is the only thing that grants.**

---

## Testing

`fetch` is resolved at **call time** (not client-creation time), so a test that spies on the global `fetch` after the module loads is intercepted normally:

```ts
vi.spyOn(globalThis, "fetch").mockResolvedValue({ ok: true, status: 200, json: async () => ({ status: "active", activationToken: "tok" }) } as Response);
const lic = await client.activate("KEY-123");
expect(lic.status).toBe("active");
```

Inject `fetchImpl` only if you need a non-global fetch. The SDK's own suites (`src/index.test.ts`, `src/refresh.test.ts`, `src/lifecycle.test.ts`, `src/package-contract.test.ts`) cover activate, refresh, the grace logic, the entitlement readers, lifecycle classification, and the npm package boundary.

<!-- cortex:docs:start -->
## Repository truth docs
- [Product overview](docs/product.md) — what this is and does (generated, code-grounded)
- [Architecture](docs/architecture.md) — components, flows, interfaces (generated, code-grounded)
<!-- cortex:docs:end -->

## Build broker

The build broker (`rustright cargo|rustc|rustdoc`) is the Rust product RustRight (repo `bogusyogi/rustright`, binary `rustright`; its old `rightkit` alias is retired); see [RustRight](../rustright/README.md). The Node broker packages were removed on 2026-10-09. `packages/build-control-policy` stays here and carries the agent permission hooks.
