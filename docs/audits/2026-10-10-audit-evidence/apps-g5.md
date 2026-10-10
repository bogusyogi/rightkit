# Apps G5 inventory: non-desktop products vs RightKit (read-only)

Date 2026-10-10. Repos: `sellright`, `rightsites`, `xtool`, `workflows/lightcraft`. No cargo, pnpm, builds or edits were run. Only `ls/wc/find/grep/git log/git status` plus one `right-release ownership --app <dir> --json` per repo.

References read: `rightkit/INDEX.md`, `rightkit/docs/CAPABILITY-MAP.md`, `rightkit/docs/ADOPTION.md`, `rightkit/packages/release/rightkit-versions.json` (`registry` section = latest published), the per-package READMEs for license, legal, logs, i18n, theme, updates, qa, git, and `swift/RightKitSwift/README.md`.

Latest published (registry section) used below: `@rightkit/legal` 0.3.1, `@rightkit/license` 0.1.8, `@rightkit/logs` 0.1.4, `@rightkit/updates` 0.2.5, `@rightkit/release` 0.2.147, `@rightkit/git` 0.2.33, `@rightkit/theme` 0.2.1, `@rightkit/i18n` 0.1.0, `@rightkit/qa` 0.4.0, `@rightkit/qa-chrome` 0.1.3, `@rightkit/hooks` 0.1.2, `@rightkit/ax` 0.2.4, `@rightkit/doc-render` 0.1.4. `rightkit-swift` registry record 0.1.1 (`rightkit-versions.json` `swift` section; staged 0.3.1; source `swift/RightKitSwift/VERSION` 0.3.8, `docs/INVENTORY-STATUS.md:100` marks it "unverified").

LOC method: git-tracked files only, `ts/tsx/mjs/cjs/js` (Swift and Rust counted separately), `prod` excludes `*.test.*` and `*.spec.*`. Built output dirs (`*/server`, `dist`) are gitignored and ignored here.

## Scanner status (all four runs refused)

| Repo | Result | JSON |
|---|---|---|
| sellright | `ok:false`, `protected-evidence`: "committed ownership: protected release checkout has tracked or untracked changes" (3 dirty paths: `AGENTS.md`, `LICENSE`, deleted `.agent/config.json`) | `apps-g5-sellright-ownership.json` |
| rightsites | same error (1 dirty path: `AGENTS.md`) | `apps-g5-rightsites-ownership.json` |
| xtool | same error (2 dirty paths: `AGENTS.md`, deleted `CLAUDE.md`); also not in `rightkit/packages/release/ownership-repositories.json` | `apps-g5-xtool-ownership.json` |
| lightcraft | `ok:false`, "repository is not registered in protected trust" (clean tree; not in `ownership-repositories.json`) | `apps-g5-lightcraft-ownership.json` |

All four have `rows: []`, `violations: []`, so the official scanner gave zero per-state counts. As a substitute I ran a **manual fingerprint sweep** (read-only Python, `scratchpad/tools/sweep.py`) that applies every `source` regex and `dependency` fingerprint from `rightkit/packages/release/rightkit-ownership.json` to each repo's tracked files. It is an approximation of the scanner (no trust, no usage-context filtering, no lockfile reachability). Hits:

| Repo | Fingerprint hits (tier) |
|---|---|
| sellright | 4: `browser.npm.playwright-test` (**block**) at `packages/storefront/package.json:45`; `theme.runtime.prefers-dark` (advise) at `packages/admin/src/theme.tsx`; `theme.math.relative-luminance` (advise, false positive) at `ios/Tests/ThemeTests.swift`; `keychain.api.secitem` (advise) at `ios/SellRightAdmin/Core/Keychain.swift` |
| rightsites | 5: no block-tier hits. `theme.runtime.prefers-dark` (`packages/admin/src/theme.tsx`); `keychain.api.secitem` (`ios/.../Keychain.swift`); `theme.math.relative-luminance` (false positive, `ios/Tests/ThemeTests.swift`); `qa.cdp.method-call` (advise, `orthiclabs/artifacts/scroll-test.mjs`, 453 LOC raw CDP driver); `platform.js.user-agent-sniff` (false positive, vendored `orthiclabs/public/vendor/ScrollTrigger.min.js`) |
| xtool | 3 (all advise): `automation.api.ax-element` (`native/macos-desktop-helper/main.swift:67`); `mcp.npm.sdk` (`package.json:33`); `pdf.npm.pdf-lib` (`package.json:38`) |
| lightcraft | 2 (advise): `sidecar.rs.json-line-read` (`apps/lightcraft-cli/tests/cli.rs`); `theme.math.relative-luminance` (false positive, colour math in `crates/color`, `crates/pipeline`) |

Resulting state counts (manual, not scanner): sellright 1 block + 3 advise (1 false positive); rightsites 0 block + 5 advise (2 false positives); xtool 0 + 3; lightcraft 0 + 2 (1 false positive).

---

## sellright

### 1. Shape

| Item | Value |
|---|---|
| Remote / visibility | `git@github.com:Orthic-Labs/sellright.git`; public repo (registered public in `ownership-repositories.json`) |
| Last commit | 2026-10-09 19:56:48 +0530 `8774314` "AGENTS.md: regenerate from workspace rules" (755 commits) |
| Classification | Orthic Labs commerce backend product (open source). Not a desktop Right Suite app; infrastructure-grade server plus iOS admin app. Uses RightApps-style licensing machinery (see gaps) |
| Manifests | `package.json` (root, `pnpm@11.25.0`, node `>=24.7 <25`), `pnpm-workspace.yaml` (`packages/*`, `!packages/admin`), per-package `package.json`, `right-release.config.mjs` (`app: "sellright"`, `deps.workdirs`), `ios/project.yml` (XcodeGen) |
| Runtime targets | Node 24 Hono API + Postgres (drizzle) in Docker compose appliance; Qwik SSR storefront (Node); React 18 + Vite admin SPA (nginx); iOS 17.2 admin app (SwiftUI, Live Activities) |

In-house packages (tracked LOC):

| Package | Prod LOC | Test LOC | Notes |
|---|---|---|---|
| `packages/api` | 39,364 | 30,114 | licensing 3,597 (+2,889 test), routes, auth 1,653, jobs 1,473, email 1,097, security 549, push 412 |
| `packages/admin` | 8,890 | 927 | standalone SPA, own lockfile |
| `packages/storefront` | 27,980 | 3,067 | Qwik; `e2e/` Playwright 237 LOC |
| `packages/storefront-client` | 16,619 | 334 | generated API client |
| `packages/shared` | 46 | 0 | |
| `ios/` (Swift) | 2,850 | (in total) | `SellRightAdmin`, `SellRightWidgets`, `Shared`, `Tests` |
| `.github/workflows` | 1,431 YAML | | `ci.yml` 1,331, `codeql.yml` 36, `refresh-dependencies.yml` 64; all hand-written |

### 2. RightKit adoption today

| Pin | Where | Version | Latest published | Status |
|---|---|---|---|---|
| `@rightkit/legal` | `packages/api/package.json:30` | `0.3.1` exact | 0.3.1 | current |
| `RightKitSwift` (product `RightKitDiagnostics`) | `ios/project.yml:17-30` (`from: 0.1.0`); resolved `ios/SellRightAdmin.xcodeproj/project.xcworkspace/xcshareddata/swiftpm/Package.resolved:10` = `0.1.0` | 0.1.0 | 0.1.1 (registry); source 0.3.8 | **stale** (resolved 0.1.0 vs 0.1.1; range `from: 0.1.0` admits 0.1.1) |
| `@rightkit/release` | none as a dependency; `right-release.config.mjs` exists | n/a | 0.2.147 | config only (see blocker below) |
| `@rightkit/license`, `logs`, `updates`, `theme`, `i18n`, `qa`, `qa-chrome`, `git`, `hooks` | none | | | not adopted |

No `.rightkit/` directory (no ownership exceptions). `right-release.config.mjs` has no `distribution` block. A public product needs `distribution.provider: "github-releases"` plus `distribution.repository` (`rightkit/packages/release/upload-release.mjs:55-59`).

### 3. Still-to-move table

| Class | Item | File / LOC | RightKit owner / note |
|---|---|---|---|
| Already on RightKit | legal manifest types + `assertLegalManifest` | `packages/api/src/legal/manifests.ts` (54), `acceptance.ts` imports types | `@rightkit/legal` |
| Already on RightKit | iOS diagnostics (JSONL + redactor) | `ios/SellRightAdmin/Core/Diagnostics.swift` (19) | `RightKitDiagnostics` |
| Duplicate (partial) | server-attested checkout receipt builder, snake_case wire shape, claim-vs-manifest equality checks | `packages/api/src/legal/acceptance.ts` 152 | `@rightkit/legal` (`createAcceptanceReceipt`, `LegalAcceptanceReceipt` is a different camelCase shape and has no server-side claim verifier, so this is also a gap) |
| Duplicate | signed artifact-manifest validation, patch/update lane rules, R2 key discipline | `packages/api/src/licensing/update-tier.ts` 192 + `runtime-artifact-manifest.ts` 176 | `@rightkit/release` `runtime-artifact-manifest.mjs` (265 LOC: `validateRuntimeArtifactManifest`, `verifyRuntimeArtifactEnvelope`) and `@rightkit/updates` `updateTierFromManifest`. Blocker: `@rightkit/release` has no `exports` map (`package.json` has only `bin`), so server TS cannot import the validator |
| Duplicate | vendored copy of the license vector | `packages/api/src/licensing/test-vectors/license-v2.json` | byte-identical (sha prefix `a4bd445c`) to `rightkit/packages/license/test-vectors/license-v2.json` and `rightkit/swift/contracts/license-v2.json`; should be consumed, not copied |
| Duplicate | admin theme system (4 themes x light/dark/system, `data-theme` + `.dark`, matchMedia) | `packages/admin/src/theme.tsx` 113 + `theme-tokens.css` 297 (generated by an untracked `.audit/themes/gen-tokens.cjs`) | `@rightkit/theme` (`applyBrand`, `generateCss`, `themeBootScript`). Partial fit: RightKit models one brand with light/dark, not four themes |
| Duplicate | Playwright e2e + CI browser install | `packages/storefront/e2e/checkout-flow.spec.ts` 133, `fixtures.ts` 50, `playwright.config.ts` 54; `@playwright/test` dep `packages/storefront/package.json:45`; `ci.yml:682` (`npx playwright install`) | `@rightkit/qa` (`QaBrowserSession`, CDP page driver) + `@rightkit/qa-chrome`. The `browser.npm.playwright-test` fingerprint is **block** tier (`rightkit-browser`) |
| Duplicate | iOS Keychain wrapper | `ios/SellRightAdmin/Core/Keychain.swift` 45 (`SecItem*`) | `RightKitAppleCredentials` (`AppleCredentialStore.read/write/deleteEverywhere`). Blocker: published `rightkit-swift` is 0.1.1; that product is documented at 0.3.8 (unverified publication) |
| Duplicate (small) | Live Activity local update/end | `ios/SellRightAdmin/Core/OrderActivityController.swift` 37 | `RightKitLiveActivity` `LiveActivityHost` (same blocker) |
| RightKit gap | server-side license authority: Ed25519 signer, activations, device leases, tiers, trials, mint, renewal, `/v1/licenses/{activate,refresh,deactivate,trial}` (`packages/api/src/routes/apps.ts:142,204,251,293`) | `licensing/` 3,597 prod + `sign.ts` 188 | `@rightkit/license` is client-only; `rightkit-license` is verifier-only. No server owner. Canonical field order in `sign.ts:35-39` must be kept identical by hand |
| RightKit gap | App Store server verification (StoreKit JWS, root certs, notifications) | `licensing/storekit-*.ts`, `apple-root-certificates.ts` ~1,283 | `RightKitPurchases` is client-side only |
| RightKit gap | Node CI template: hand-written pnpm CI with DB, packaging, update-compat, appliance jobs | `.github/workflows/ci.yml` 1,331, codeql 36, refresh 64 | `@rightkit/git` has only the `rust-hybrid` profile (`packages/git/templates/profiles/`) |
| RightKit gap | web i18n | none (UI strings inline; `packages/storefront/src/utils/i18n.ts` is an 8-line base-path helper, not i18n) | `@rightkit/i18n` is React-only; the Qwik storefront cannot use it |
| RightKit gap | server logging | `packages/api/src/lib/logger.ts` 81 (pino) | `@rightkit/logs` is a Tauri webview client |
| RightKit gap | server OIDC (Apple sign-in 122 LOC `auth/apple.ts`; Google ID-token check `routes/auth.ts:67-77`) | | `rightkit-oauth` is desktop loopback PKCE / device flow in Rust |
| RightKit gap | a11y audit helper (axe) | `packages/admin/src/qa-a11y.ts` 101 | `@rightkit/qa` has no axe module |
| Not applicable | commerce domain, payments, drizzle schema, email, jobs, push/APNs (412), shipping, SEO feeds, rate limiting, secret-crypto (549 security dir), `lib/sanitize-html.ts` (34), storefront, generated client, deploy appliance | | no RightKit owner (`rightkit-jobs`, `rightkit-cache`, `rightkit-http` are Rust) |

### 4. Ownership scanner summary

Refused: dirty protected checkout (3 modified/deleted paths). Zero rows. Manual sweep: 1 block (`@playwright/test`), 3 advise (1 false positive). See the top table.

### 5. Handover list (sellright)

1. Resolve scanner admission first: commit or stash `AGENTS.md`, `LICENSE`, `.agent/config.json`, then rerun; expected live finding is the block-tier Playwright dependency. Blocker: Adrian owns the commit.
2. Replace Playwright e2e + `@playwright/test` with `@rightkit/qa@0.4.0` + `@rightkit/qa-chrome@0.1.3`; est. 240 LOC rewrite plus `ci.yml:682`; blocker: none (published), must clear the block-tier fingerprint.
3. Replace `acceptance.ts` equality checks with a `@rightkit/legal@0.3.1` server-claim verifier; est. 150 LOC; blocker: RightKit gap, add `verifyAcceptanceClaim` (checkout-role subset, snake_case wire receipt) to `@rightkit/legal`.
4. Replace `update-tier.ts` + `runtime-artifact-manifest.ts` + the vendored `license-v2.json` with `@rightkit/release@0.2.147` / `@rightkit/updates@0.2.5` / `@rightkit/license@0.1.8` exports; est. 370 LOC + vector; blocker: RightKit must add package `exports` (or a small `@rightkit/contracts-node` subpath).
5. Replace `ios/.../Keychain.swift` (45) and `OrderActivityController.swift` (37) with `RightKitAppleCredentials` / `RightKitLiveActivity` from `rightkit-swift`; bump `ios/project.yml:19` and `Package.resolved`; est. 82 LOC; blocker: `rightkit-swift` 0.1.1 is the latest published, products need the staged/source line (0.3.x) published.
6. Add `distribution.provider: "github-releases"` + repository to `right-release.config.mjs`; est. 5 LOC; blocker: confirm sellright is released through GitHub Releases at all (it ships a Docker appliance).
7. Replace `packages/admin/src/theme.tsx` + generated tokens with `@rightkit/theme@0.2.1`; est. 410 LOC; blocker: four-theme model vs brand-file model (RightKit gap or design decision).
8. Adopt right-git for CI; est. 1,431 LOC of workflow; blocker: no node/pnpm profile in `@rightkit/git@0.2.33`.

---

## rightsites

### 1. Shape

| Item | Value |
|---|---|
| Remote / visibility | `git@github.com:bogusyogi/rightsites.git`; private (registered private) |
| Last commit | 2026-10-09 19:56:42 +0530 `6612d4de` (1,228 commits); 1 dirty path (`AGENTS.md`) |
| Classification | Infrastructure. Web-layer fork of SellRight; hosts the RightApps backend (`rightsites-api`, PM2 on Hetzner: licenses, release registration, model delivery) plus the marketing/commerce sites for CodeRight, HeardRight, MailRight, ScrapeRight, ViewRight and OrthicLabs. Not itself a Right Suite product |
| Manifests | root `package.json` (`pnpm@11.25.0`), `pnpm-workspace.yaml`, `right-release.config.mjs` (`app: "sellright"`, `distribution.provider: "r2-rightsites"`), one `package.json` per site (6) plus per package, `workers/*/wrangler.toml`, `ios/project.yml` |
| Runtime targets | Node API (Hono, Postgres), 6 Qwik SSR express sites (PM2 via `serve.mjs`), admin SPA, 4 Cloudflare Workers, iOS admin app (identical 2,850-LOC copy of sellright's `ios/`) |

In-house packages and sites (tracked LOC, ts/js):

| Path | LOC (prod/test or total) | Notes |
|---|---|---|
| `packages/api` | 43,794 / 34,303 | `rightsites-plugin/` 3,103 / 1,820 (pro-devices 541, telemetry 878, release-routes 385, runtime-artifact-resolve 279); `licensing/` 4,265 / 4,243 |
| `packages/admin` | 8,857 / 897 | |
| `packages/storefront` | 42,471 / 992 | package name `damned-designs-storefront` |
| `packages/storefront-apps` | 2,265 / 0 | brand/checkout shell for the 5 apps |
| `packages/storefront-client` | 16,596 / 334 | |
| `workers/` | `license-gate` 282/443, `heardright-models` 621/403, `shared` 93, `scraperight-waitlist` 239 | Cloudflare Workers |
| Sites: `coderight` 5,635, `heardright` 10,991, `mailright` 5,123, `scraperight` 5,439, `viewright` 6,764, `orthiclabs` 4,563 | | `server/` dirs are gitignored build output; the real sources are `src/`, `scripts/`, `adapters/` |
| `deploy/` | 1,514 | compose, Caddy, install |
| `.github` | only `pull_request_template.md` | no workflows |

### 2. RightKit adoption today

| Pin | Where | Version | Latest published | Status |
|---|---|---|---|---|
| `@rightkit/legal` | `packages/api/package.json:31` | `0.3.1` exact | 0.3.1 | current |
| `RightKitSwift` `RightKitDiagnostics` | `ios/project.yml:17-30` (`from: 0.1.0`) | 0.1.0 | 0.1.1 | stale (same as sellright; no tracked Package.resolved) |
| `@rightkit/release` exclusion list | `pnpm-workspace.yaml:55` `minimumReleaseAgeExclude` lists `0.2.9 \|\| 0.2.10 \|\| 0.2.11 \|\| 0.2.44 \|\| 0.2.63 \|\| 0.2.65 \|\| 0.2.66 \|\| 0.2.92` | | 0.2.147 | **stale** (not a dependency; all eight versions are older than latest) |
| `@rightkit/legal` exclusion list | `pnpm-workspace.yaml:56` `0.1.0 \|\| 0.2.0` | | 0.3.1 | **stale** (current dependency 0.3.1 is not excluded) |
| `distribution.provider` | `right-release.config.mjs:4` = `r2-rightsites` | | `publish-update.mjs:47` requires `r2-rightapps`; `upload-release.mjs:55` requires `github-releases` | **unknown provider name** (rename drift, release would fail closed) |
| Ownership exceptions | `.rightkit/ownership-exceptions.json:5-16` `oauth.endpoint.google` on `packages/api/src/routes/auth.ts`; `:17-28` `secrets.api.secitem` on `ios/SellRightAdmin/Core/Keychain.swift` | | | first fingerprint no longer exists (`crates/rightkit-oauth/rightkit-ownership.json` has only `oauth.cargo.*` / `oauth.npm.*` dependency fingerprints), so it looks stale; second still matches |

No site (`coderight`, `heardright`, `mailright`, `scraperight`, `viewright`, `orthiclabs`) declares any `@rightkit/*` dependency. The `THIRD_PARTY_NOTICES.md` in each site says it was generated by "`@rightkit/legal` notice contract", and `*/public/legal-bundle/THIRD_PARTY_RUST_LICENSES.html` lists `rightkit-license 0.1.2` and `rightkit-tauri 0.1.0` (the apps' resolved crates, behind current 0.1.3 / 0.1.2).

### 3. Still-to-move table

| Class | Item | File / LOC | RightKit owner / note |
|---|---|---|---|
| Already on RightKit | legal manifest types + validation | `packages/api/src/legal/manifests.ts` (54), `manifests/index.ts` (40) plus 5 `manifests/*.json` | `@rightkit/legal` |
| Already on RightKit | iOS diagnostics | `ios/SellRightAdmin/Core/Diagnostics.swift` (19) | `RightKitDiagnostics` |
| Inherited duplicates (same files as sellright) | `legal/acceptance.ts` 152; `licensing/update-tier.ts` 192; `licensing/runtime-artifact-manifest.ts` 183; `licensing/test-vectors/license-v2.json` (identical sha to RightKit); `packages/admin/src/theme.tsx` 113 + `theme-tokens.css` 297; `ios/.../Keychain.swift` 45; `ios/.../OrderActivityController.swift` 37 | see sellright | same owners and blockers |
| Duplicate | 5 near-identical site legal publishing tests (hash-bound manifest vs `src/legal` vs `public/legal-bundle` vs checkout contract) | `{coderight,heardright,mailright,scraperight,viewright}/scripts/test-legal-publishing-contract.mjs` 71+66+64+66+65 = 332 | `@rightkit/legal` (`assertLegalManifest`) and `@rightkit/release` `legal-contract.mjs` (`assertLegalReleaseContract`, 168 LOC) |
| Duplicate | hand-synced copy of canonical manifests for checkout | `packages/storefront-apps/src/lib/legal-contracts.ts` 85 (comment at line 19 says "must be resynced whenever a manifest changes") | `@rightkit/legal`; gap: no generator/serializer for the checkout subset |
| Duplicate | third Ed25519 token verifier in WebCrypto + revocation cache | `workers/license-gate/src/index.ts` 252 (verify at `:128-142`), `workers/shared/revocations.ts` 93, `packages/api/src/rightsites-plugin/license-revocations.ts` 57 | `@rightkit/license` has no verifier (offline verify is native Rust/Swift only). RightKit gap, see below |
| Duplicate (raw CDP) | scroll test via CDP | `orthiclabs/artifacts/scroll-test.mjs` 453 | `@rightkit/qa` CDP page driver |
| RightKit gap | server-side license/entitlement authority and the RightApps backend: `licensing/` 4,265 prod, `rightsites-plugin/` entitlement-provider 154, pro-devices 541, pro-bridge 186, license-admin-routes 68, release-routes 385, runtime-artifact-resolve 279, release-auth 54 | ~6,000 | `@rightkit/license` client and `@rightkit/release` `rightapps-register.mjs` call it; the service itself has no RightKit owner (`docs/RUNTIME-ARTIFACT-DELIVERY.md` documents it as RightSites code) |
| RightKit gap | StoreKit server verification | `licensing/storekit-*.ts`, `apple-root-certificates.ts` ~1,300 | `RightKitPurchases` is client-only |
| RightKit gap | private-repo Node CI | rightsites has no workflows; `@rightkit/git` is public-repo-only (README: "install and sync refuse private ... repositories") and rust-hybrid only | no owner |
| RightKit gap | server logging / web i18n / OIDC / a11y | same as sellright | |
| Not applicable | commerce backend, 6 Qwik sites (`serve.mjs` x6, vite/tailwind configs, `purge-cloudflare-cache.mjs` x2 identical 70 LOC, SEO scripts), `workers/heardright-models` (model grants, 621), `workers/scraperight-waitlist`, `deploy/` | | Cloudflare/PM2 hosting, SEO and content; no RightKit owner. `heardright/src/lib/platform.mjs` (23) is website UA sniffing, not `@rightkit/platform-ui` (Tauri-backed) |

### 4. Ownership scanner summary

Refused: dirty protected checkout (1 path). Zero rows. Manual sweep: 0 block, 5 advise, 2 of them false positives; `ownership-exceptions.json` records 2 approved exceptions (see adoption table).

### 5. Handover list (rightsites)

1. Fix `right-release.config.mjs:4` provider `r2-rightsites` to a name `@rightkit/release@0.2.147` accepts (`r2-rightapps`), or have RightKit add the new name (RightApps to RightSites rename); est. 1 LOC; blocker: Adrian decides which side renames.
2. Replace the 5 `test-legal-publishing-contract.mjs` plus `legal-contracts.ts` with one shared check/generator on `@rightkit/legal@0.3.1` / `@rightkit/release` `legal-contract`; est. 417 LOC to ~60; blocker: RightKit gap, add a checkout-subset generator and a `verify-published-bundle` helper.
3. Replace `workers/license-gate` verifier (252 + 93 + 57) with an `@rightkit/license` WebCrypto verifier; est. 400 LOC; blocker: RightKit gap, no TS verifier (README: offline verify is native); vector `test-vectors/license-v2.json` already shared.
4. Move runtime-artifact and update-tier validation (192 + 183) onto `@rightkit/release` / `@rightkit/updates`; est. 375 LOC; blocker: no `exports` map.
5. Decide ownership of the server-side license service (~6,000 LOC + StoreKit ~1,300): either declare rightsites as the owner of `licensing` (server) or create `@rightkit/license-server`; blocker: Adrian (RightKit gap).
6. Clean stale config: `pnpm-workspace.yaml:55-56` exclusion versions; `.rightkit/ownership-exceptions.json` stale `oauth.endpoint.google` exception (owner approval needed, agents may not edit); est. 6 LOC; blocker: exception edits need Adrian.
7. Replace `orthiclabs/artifacts/scroll-test.mjs` with `@rightkit/qa`; est. 453 LOC; low priority.
8. iOS: shared with sellright items 5 and theme item 7. `ios/` is a byte-for-byte duplicate of sellright's `ios/`; keep one.

---

## xtool

### 1. Shape

| Item | Value |
|---|---|
| Remote / visibility | `git@github.com:bogusyogi/xtool.git`; not in `ownership-repositories.json`; MIT |
| Last commit | 2026-09-09 18:43:19 +0530 `17dcc15` "Add immediate Codex task control" (124 commits); 2 dirty paths (`AGENTS.md`, deleted `CLAUDE.md`) |
| Classification | Standalone infrastructure tool (headless ChatGPT-to-laptop MCP bridge). Not a Right Suite product |
| Manifests | `package.json` (v2.1.0, `pnpm@11.18.0`, below the workspace default 11.24.0; no `engines`), `pnpm-workspace.yaml`, `vitest*.config.ts`, `tsconfig.json` |
| Runtime targets | single persistent Node host (Windows and macOS `launchd`), plus a macOS native helper (`native/macos-desktop-helper/main.swift` 1,656 LOC, `addon.cc`), vendored Desktop Commander handlers (`vendor/desktop-commander` 1,502) |
| In-house LOC | `src` 30,995 prod (session `store.ts` 2,442 + `recorder.ts` 2,331; `computer/` 2,792; `mcp/tools-core.ts` 1,795; `exec-hints.ts` 1,597; `codex/unified-exec.ts` 1,092; `tunnel/` 1,131); tests 19,293; `scripts` 1,817; no `.github/workflows` (issue templates + PR template only); `.githooks/` 3 files |

### 2. RightKit adoption today

None. No `@rightkit/*` or `rightkit-*` pin in `package.json`, lockfile-free grep of the tree finds no reference (the only "RightKit" mention is `AGENTS.md`). Not in the ownership registry. Stale pins: none (nothing pinned). `packageManager` pnpm 11.18.0 vs workspace pnpm 11.24.0 (`rightkit-versions.json`).

### 3. Still-to-move table

| Class | Item | File / LOC | RightKit owner / note |
|---|---|---|---|
| Duplicate (Node vs Rust owners) | OS credential store: DPAPI / Keychain / libsecret codec | `src/main/secrets.ts` 363 | `rightkit-secrets` (Rust only; no Node owner) |
| Duplicate (Node vs Rust owners) | path sandbox with realpath containment | `src/main/sandbox.ts` 490, `fsops.ts` 705 | `rightkit-sandbox` / `rightkit-fs` (Rust only) |
| Duplicate (Node vs Rust owners) | PTY / exec / process lifecycle | `src/main/exec.ts` 591, `codex/unified-exec.ts` 1,092 | `rightkit-process` / `rightkit-sandbox` PTY lease (Rust only) |
| Duplicate (Node vs Rust owners) | atomic durable JSON | `src/main/durable.ts` 192 | `rightkit-fs` (Rust only) |
| Duplicate (Node vs Rust owners) | macOS desktop observation/control (AX) | `native/macos-desktop-helper/main.swift` 1,656 (`AXUIElement*` at `:67`), `computer/` 2,792 | `rightkit-control` / `rightkit-automation` (Rust; `rightkit-automation` staged 0.1.0, unpublished) |
| Duplicate (Node vs Rust owners) | MCP server host (stdio/HTTP, tool admission) | `src/main/mcp/` 17 files (kernel 328, server 517, tools-core 1,795); `@modelcontextprotocol/sdk` 1.29.0 `package.json:33` | `rightkit-mcp` 0.5.2 (Rust). `@rightkit/ax` 0.2.4 (AXA-1 gates over an MCP surface) applies as a test gate |
| Duplicate (Node vs Rust owners) | PDF read/write | `src/main/documents.ts` 227, `pdf-lib` `package.json:38` | `rightkit-pdf` (Rust) |
| Not applicable | logger 138 LOC (`src/main/logger.ts`) | | `@rightkit/logs` is a Tauri webview client |
| Not applicable | Codex/Pi/Claude bridge and session store, tunnel client, tools, search (ripgrep) | | no RightKit owner |
| RightKit gap | Node equivalents of secrets, sandbox/fs, process, MCP host; `@rightkit/hooks` (lifecycle runtime) is the only Node package near `.githooks`, no CI template (`@rightkit/git` is public + rust-hybrid only, xtool has no workflows) | | the RightKit Rust crates are unusable from a Node host without a native binding |

Duplicates: 7 areas, ~8,000 LOC of Node/Swift that overlaps Rust owners; none is directly replaceable by an npm package.

### 4. Ownership scanner summary

Refused: dirty protected checkout (2 paths); repo is also not registered. Manual sweep: 3 advise hits (`automation.api.ax-element`, `mcp.npm.sdk`, `pdf.npm.pdf-lib`).

### 5. Handover list (xtool)

1. Decide scope: either register xtool in `ownership-repositories.json` (Adrian-only) or record it as out-of-suite; est. 0 LOC; blocker: Adrian.
2. Replace `native/macos-desktop-helper` AX code (1,656) with a `rightkit-control` / `rightkit-automation` sidecar; est. 1,656 + `computer/` 2,792; blocker: `rightkit-automation` unpublished; needs a Node-callable surface (`rightkit-service` socket).
3. Replace `secrets.ts` (363) via a `rightkit-secrets` helper over `rightkit-service`; est. 363; blocker: RightKit gap (no Node binding).
4. Replace `mcp/` server host with `rightkit-mcp@0.5.2` via sidecar, or keep and add `@rightkit/ax@0.2.4` gates; est. 3,400; blocker: language boundary (xtool is Node).
5. Align `packageManager` to 11.24.0 and add `engines`; est. 2 LOC; blocker: none.

---

## workflows/lightcraft

### 1. Shape

| Item | Value |
|---|---|
| Remote | `git@github.com:Orthic-Labs/lightcraft.git`; not in `ownership-repositories.json` |
| Last commit | 2026-10-08 04:45:28 +0530 `fd6a998` "ci: register generated native candidate dispatch" (553 commits); tree clean |
| Classification | Tool (photo editor/RAW developer: egui desktop app, CLI, wasm web build, MCP server). Brand `ai.storyteller.lightcraft`; not a Right Suite desktop app |
| Manifests | `Cargo.toml` workspace (v0.2.1, Rust 1.90, edition 2024), `Cargo.lock`, `xtask/`; **no `package.json`, pnpm lock, `.rightgit.json` or `right-release.config.mjs` tracked** |
| Rust LOC | `crates/engine` 25,946; `ui-egui` 23,676; `raw` 9,855; `pipeline` 7,536; `catalog` 6,765; `codecs` 4,387; `meta` 3,912; `merge` 3,843; `gpu` 3,340; `mcp` 1,594; others; apps `lightcraft` 1,430, `lightcraft-cli` 1,105, `lightcraft-web` 3,067; `xtask` 2,056. Packaging scripts 803 (`packaging/{linux,macos,windows,web}`) |
| Workflows | 738 YAML: `release-candidate.yml` 207 (right-git managed), `release.yml` 324, `windows-arm64.yml` 95, `freebsd.yml` 55, `packaging-lint.yml` 57 |

### 2. RightKit adoption today

| Item | Where | Value | Status |
|---|---|---|---|
| `right-git` lane | `.github/workflows/release-candidate.yml:1-2` | "Managed by right-git", template 1.8.0 | current (`packages/git/install.mjs:16` `DEFAULT_TEMPLATE_VERSION = "1.8.0"`) but **incomplete**: it runs `pnpm install --frozen-lockfile`, `pnpm exec right-release ownership --app .` (`:84-86`) and `pnpm run release:admit`, while the repo has no `package.json`, lockfile, `.rightgit.json`, `release:admit` script or gate, so the lane cannot run |
| Cargo pins | none (`grep rightkit` in all `Cargo.toml` finds nothing; `Cargo.lock` has 0 `rightkit*` packages) | | no adoption |
| `@rightkit/*` | none | | no adoption |

### 3. Still-to-move table (brief)

| Class | Item | File / LOC | RightKit owner |
|---|---|---|---|
| Duplicate | hand-written signing/notarization/installer pipeline: macOS cert import, notarize, Windows Azure signing, WiX, nfpm/flatpak | `.github/workflows/release.yml` 324, `windows-arm64.yml` 95, `packaging/macos/{package,import-cert}.sh` 215, `packaging/windows/{package,sign}.ps1` 226, `packaging/linux/package.sh` 120; `xtask/src/version.rs` 152 | `@rightkit/release` (`right-release`) + `@rightkit/git`. Conflicts with workspace rules (no signing machinery in a product repo; `windows-arm64.yml:1,35` cross-compiles on x64) |
| Duplicate | stdio JSON-RPC MCP server (hand-written, no async runtime) | `crates/mcp` 1,439 (+ `docs/mcp.md`) | `rightkit-mcp` 0.5.2 (stdio + loopback HTTP host) |
| Duplicate | bounded loopback JSON-lines control channel | `apps/lightcraft/src/control_server.rs` 267, `crates/ui-egui/src/control.rs` 408, `docs/control-protocol.md` | `rightkit-framed-sidecar` 0.1.0 / `rightkit-service` 0.1.3 (sweep hit: `sidecar.rs.json-line-read`) |
| Duplicate | cross-process library lock (`flock`/`LockFileEx`) and atomic writes | `crates/catalog/src/lock.rs` 140, `store.rs` `write_atomic` | `rightkit-fs` 0.3.3 (locks, atomic writes) |
| Duplicate | panic guard / last-resort crash handling | `crates/engine/src/guard.rs` 55 | `rightkit-crash` 0.2.0 (partial) |
| RightKit gap | Rust UI localization (ja, zh-Hans, zh-Hant, 3,676 JSON lines) | `crates/ui-egui/src/i18n.rs` 730 | `@rightkit/i18n` is React-only; no Rust i18n owner |
| Not applicable | RAW decode, colour, GPU pipeline, catalog format, egui UI, xtask asset/parity tools | | no RightKit owner |

### 4. Ownership scanner summary

Refused: "repository is not registered in protected trust". Manual sweep: 2 advise hits (1 false positive).

### 5. Handover list (lightcraft)

1. Decide registration and the Node harness: either add `package.json` + `.rightgit.json` + `release:admit` (so `release-candidate.yml` runs) or drop the managed lane; blocker: Adrian (register in `ownership-repositories.json`; workflow invokes tooling the repo does not have).
2. Retire `release.yml` / `windows-arm64.yml` / `packaging/*/sign*` for `right-release@0.2.147` native-host signing; est. ~1,100 LOC; blocker: native-host build rule (no cross-compile) and a Linux/FreeBSD/web story that RightKit does not cover.
3. Replace `crates/mcp` with `rightkit-mcp@0.5.2`; est. 1,439 LOC; blocker: Rust-side adapters and RightKit cargo access via `rightkit cargo`.
4. Move `catalog/lock.rs` and `write_atomic` to `rightkit-fs@0.3.3`; est. ~200 LOC; blocker: none beyond Rust toolchain rule.

---

## Cross-repo RightKit gaps (summary)

1. Server-side license authority (issuer, activations, leases, StoreKit verification, release/model registration): ~6,000 LOC in rightsites and ~3,600 in sellright, no RightKit owner. `@rightkit/license` is client-only and `rightkit-license` verifier-only.
2. TS/WebCrypto license-token verifier (rightsites `workers/license-gate`).
3. `@rightkit/release` exposes no `exports`, so runtime-artifact manifest validation and update-tier logic cannot be imported by server TS.
4. `@rightkit/legal` has no server-side checkout-claim verifier or checkout-subset generator.
5. Node/pnpm CI profile and private-repo CI in `@rightkit/git` (public + rust-hybrid only).
6. Web i18n for non-React (Qwik) and Rust i18n; server logger; server-side OIDC; axe a11y helper in `@rightkit/qa`.
7. Node bindings for the Rust owners xtool duplicates (secrets, sandbox/fs, process, MCP).
8. `rightkit-swift` publication lag: registry 0.1.1 vs source 0.3.8; AppleCredentials / LiveActivity are not in the published version.

Artifacts: scanner JSON in `scratchpad/audit/apps-g5-{sellright,rightsites,xtool,lightcraft}-ownership.json`; sweep script `scratchpad/tools/sweep.py`.
