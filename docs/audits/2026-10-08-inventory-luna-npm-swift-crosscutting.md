# RightKit inventory report — evidence gathered

**Status: incomplete.** I did not modify files or read `docs/audits/`. The workspace is read-only, so I’m delivering this report inline; I could not save a report file. The requested package and Swift dependency-use audit, per-target dependency totals, full pnpm duplicate-version analysis, and complete cross-cutting review were not finished. I’m marking those gaps explicitly rather than presenting estimates as measured results.

## Scope & source-of-truth findings

The checked-in generated index reports 47 Rust crates and 28 npm packages. It is stale against current source: it gives older versions for numerous crates and packages, while `packages/release/rightkit-versions.json` contains the current publication registry and staged versions. The index generator reads source manifests and ownership declarations, then writes `INDEX.md`; it is suitable as a catalog, not as current version evidence ([`INDEX.md:5`](</Volumes/D/claude/rightkit/INDEX.md:5>), [`INDEX.md:57`](</Volumes/D/claude/rightkit/INDEX.md:57>), [`scripts/generate-index.mjs:24`](</Volumes/D/claude/rightkit/scripts/generate-index.mjs:24>), [`scripts/generate-index.mjs:39`](</Volumes/D/claude/rightkit/scripts/generate-index.mjs:39>), [`scripts/generate-index.mjs:73`](</Volumes/D/claude/rightkit/scripts/generate-index.mjs:73>)).

`rightkit-versions.json` separates published versions from staged versions. For npm, its `npm` map lists nine currently published packages; `stagedNpm` lists staged candidates. Private build-control packages and `agent-room-mcp` are not consumer packages ([`rightkit-versions.json:4`](</Volumes/D/claude/rightkit/packages/release/rightkit-versions.json:4>), [`rightkit-versions.json:15`](</Volumes/D/claude/rightkit/packages/release/rightkit-versions.json:15>)). The file also records Cargo and Swift publication/staging maps ([`rightkit-versions.json:188`](</Volumes/D/claude/rightkit/packages/release/rightkit-versions.json:188>), [`rightkit-versions.json:240`](</Volumes/D/claude/rightkit/packages/release/rightkit-versions.json:240>)).

The Swift package declares iOS 17 and macOS 14, 14 library products/targets, and no third-party SwiftPM package dependencies. Speech includes a copied resource; Artifacts links system `zlib` ([`Package.swift:1`](</Volumes/D/claude/rightkit/swift/RightKitSwift/Package.swift:1>), [`Package.swift:7`](</Volumes/D/claude/rightkit/swift/RightKitSwift/Package.swift:7>), [`Package.swift:21`](</Volumes/D/claude/rightkit/swift/RightKitSwift/Package.swift:21>), [`Package.swift:53`](</Volumes/D/claude/rightkit/swift/RightKitSwift/Package.swift:53>)).

The root pnpm workspace includes every `packages/*` directory, enables workspace linking/preference, overrides `@wdio/native-utils`, and disables native install scripts for canvas, EdgeDriver, and GeckoDriver ([`pnpm-workspace.yaml:1`](</Volumes/D/claude/rightkit/pnpm-workspace.yaml:1>), [`pnpm-workspace.yaml:5`](</Volumes/D/claude/rightkit/pnpm-workspace.yaml:5>), [`pnpm-workspace.yaml:8`](</Volumes/D/claude/rightkit/pnpm-workspace.yaml:8>)). `pnpm-lock.yaml` contains multiple versions for at least `aria-query`, `commander`, `cose-base`, `d3-array`, `d3-path`, `d3-shape`, `dom-accessibility-api`, `esbuild`, `jsdom`, `katex`, `layout-base`, and `vite`; this is a partial observation, not a complete duplicate list ([`pnpm-lock.yaml:1515`](</Volumes/D/claude/rightkit/pnpm-lock.yaml:1515>), [`pnpm-lock.yaml:1590`](</Volumes/D/claude/rightkit/pnpm-lock.yaml:1590>), [`pnpm-lock.yaml:1602`](</Volumes/D/claude/rightkit/pnpm-lock.yaml:1602>), [`pnpm-lock.yaml:1632`](</Volumes/D/claude/rightkit/pnpm-lock.yaml:1632>), [`pnpm-lock.yaml:1704`](</Volumes/D/claude/rightkit/pnpm-lock.yaml:1704>), [`pnpm-lock.yaml:1738`](</Volumes/D/claude/rightkit/pnpm-lock.yaml:1738>), [`pnpm-lock.yaml:1826`](</Volumes/D/claude/rightkit/pnpm-lock.yaml:1826>), [`pnpm-lock.yaml:3901`](</Volumes/D/claude/rightkit/pnpm-lock.yaml:3901>), [`pnpm-lock.yaml:4149`](</Volumes/D/claude/rightkit/pnpm-lock.yaml:4149>), [`pnpm-lock.yaml:4211`](</Volumes/D/claude/rightkit/pnpm-lock.yaml:4211>), [`pnpm-lock.yaml:4221`](</Volumes/D/claude/rightkit/pnpm-lock.yaml:4221>), [`pnpm-lock.yaml:5160`](</Volumes/D/claude/rightkit/pnpm-lock.yaml:5160>)).

## A–F. npm packages

Status below comes from source package versions compared with the publication registry. “Staged” means listed under `stagedNpm`; “published” means listed under `npm`; private build-control packages are not independently published. **Direct dependency lists are from manifests; use-site audits, per-dependency verdicts, transitive counts, and complete consumer pin reconciliation remain unmeasured.** The catalog descriptions and package counts are in [`INDEX.md:57–88`](</Volumes/D/claude/rightkit/INDEX.md:57>), with source status in [`rightkit-versions.json:4–37`](</Volumes/D/claude/rightkit/packages/release/rightkit-versions.json:4>).

| Package | Source version; state | Responsibility / exports in brief | Direct deps / notable peers | Consumers found in requested app manifest search |
|---|---|---|---|---|
| `@rightkit/agent-room-mcp` | `0.1.0`; private | Agent-room MCP package; source responsibility not established | none | none found |
| `@rightkit/app-shell` | `0.2.1`; staged | React app frame, palette, shortcuts, dialogs, UI primitives; root and `./react` exports | peers: React, `@rightkit/platform-ui` | CutRight, GenRight, ScrapeRight, HeardRight, Cockpit |
| `@rightkit/ax` | `0.2.4`; staged | `right-ax`; plugin validation and static/conformance/behavioral/adversarial/report gates | none | Legion, CodeRight context |
| `@rightkit/build-control-broker` | `0.1.0-phase1`; private | Build broker and native lifecycle helpers; broker/cutover/bootstrap CLIs | internal `build-control-clients`, `build-control-core`; `smol-toml` | none |
| `@rightkit/build-control-clients` | `0.1.0-phase1`; private | `rightkit` entrypoint, managed-agent and storage clients | internal `build-control-core` | none |
| `@rightkit/build-control-core` | `0.1.0-phase1`; private | Shared build-control policy/runtime primitives | none | none |
| `@rightkit/build-control-phase0` | `0.0.0-phase0`; private | Phase-0 feasibility evidence CLI | dev: `ajv` | none |
| `@rightkit/build-control-policy` | `0.1.0-phase1`; private | Agent write/session ownership policy hooks | none declared | none |
| `@rightkit/build-control-staging` | `0.1.0-phase1`; private | External staging and sync-back contract | internal `build-control-core` | none |
| `@rightkit/doc-render` | `0.1.3`; staged | Markdown/sanitized HTML rendering; root export | 12 direct runtime deps: mdast types, DOMPurify, slugger, KaTeX, Mermaid, remark plugins, Shiki, unified; React peer | ViewRight |
| `@rightkit/git` | `0.2.26`; staged | `right-git`; public-repository workflow install/sync/drift/adopt/uninstall | none | Legion, CodeRight context |
| `@rightkit/hooks` | `0.1.2`; staged | Neutral lifecycle runtime | none | Legion, CodeRight context |
| `@rightkit/i18n` | `0.1.0`; staged | Locale catalog/provider | React peer | none found |
| `@rightkit/legal` | `0.3.1`; staged | Legal manifest, assent receipt, release-contract primitives | none | CutRight, ScrapeRight, MailRight, RightSites, SellRight, CodeRight, ViewRight, HeardRight |
| `@rightkit/legal-ui` | `0.1.3`; staged | React legal acceptance gate | React peer | CutRight, ScrapeRight, ViewRight, HeardRight |
| `@rightkit/license` | `0.1.8`; staged | License activation/network/storage/grace client | `@tauri-apps/plugin-http` peer | CutRight, ScrapeRight, MailRight, CodeRight, ViewRight, HeardRight |
| `@rightkit/logs` | `0.1.4`; staged | Local structured logging and diagnostics | none | CutRight, ScrapeRight, MailRight, CodeRight, ViewRight, HeardRight |
| `@rightkit/media-ui` | `0.2.6`; staged | Media player and image viewer/editor; root, `./image`, CSS exports | runtime `react-image-crop`; peers: Fabric, React, React DOM | CutRight, GenRight, ScrapeRight, ViewRight |
| `@rightkit/onboarding` | `0.2.3`; staged | First-run onboarding framework | peers: app-shell, legal-ui, React | CutRight, GenRight, ScrapeRight, HeardRight |
| `@rightkit/pdf-ui` | `0.1.2`; staged | PDF viewer/search/link helpers; root and CSS exports | runtime `pdfjs-dist`; React peer | ViewRight |
| `@rightkit/platform-ui` | `0.1.2`; staged | Platform, shortcut, capability and window-control primitives | peers: Tauri OS plugin, React | CutRight, GenRight, ScrapeRight, Cockpit |
| `@rightkit/qa-chrome` | `0.1.3`; staged | Chrome-for-Testing/CDP harness | none | ScrapeRight, ViewRight, HeardRight |
| `@rightkit/qa` | `0.4.0`; staged | QA harness/evidence/storage primitives; root and `./chrome` exports | runtime `@rightkit/qa-chrome` | no matching public app manifest pin found; package states private in README |
| `@rightkit/release` | `0.2.135`; staged | `right-release`; build, sign, verify, publish, ownership and artifact adoption flows | runtime `smol-toml` | all listed apps/repos with versions varying; examples include CutRight, ScrapeRight, MailRight, CodeRight, ViewRight, Legion, Cockpit, VoiceRight |
| `@rightkit/settings-ui` | `0.2.7`; staged | Standard settings sections and frame | peers: app-shell, shell, license, logs, theme, updates, React | CutRight, GenRight, ScrapeRight, HeardRight |
| `@rightkit/shell` | `0.1.0`; staged | Typed renderer bridge for native shell commands | none | CutRight, CodeRight |
| `@rightkit/tauri` | `0.1.1`; staged | Typed Tauri host adapters | runtime `@rightkit/updates` | ScrapeRight, MailRight, CodeRight, ViewRight, HeardRight |
| `@rightkit/theme` | `0.2.1`; staged | Theme tokens and runtime brand application | none | CutRight, GenRight, ScrapeRight, HeardRight |

Examples verified in source include `@rightkit/qa` re-exporting `@rightkit/qa-chrome` ([`packages/qa/src/chrome.ts:1`](</Volumes/D/claude/rightkit/packages/qa/src/chrome.ts:1>)), `@rightkit/tauri` importing `@rightkit/updates` ([`packages/tauri/src/index.ts:1`](</Volumes/D/claude/rightkit/packages/tauri/src/index.ts:1>)), and PDF/media packages importing their manifest runtime dependencies ([`packages/pdf-ui/src/PdfViewer.tsx:13`](</Volumes/D/claude/rightkit/packages/pdf-ui/src/PdfViewer.tsx:13>), [`packages/media-ui/src/image/ImageCropTool.tsx:2`](</Volumes/D/claude/rightkit/packages/media-ui/src/image/ImageCropTool.tsx:2>)). No dependency has been labeled `UNUSED`; whole-package greps for every dependency were not completed. Transitive default/minimal-feature totals were not measured.

## A–F. SwiftPM targets

All 14 targets are library products, platform-limited to iOS 17/macOS 14, and declared in one package. The package has no external SwiftPM dependencies. Product/target evidence is in [`Package.swift:7`](</Volumes/D/claude/rightkit/swift/RightKitSwift/Package.swift:7>) and [`Package.swift:21`](</Volumes/D/claude/rightkit/swift/RightKitSwift/Package.swift:21>).

| Target | Responsibility / surface in brief | Internal dependency | App evidence found |
|---|---|---|---|
| `RightKitLicense` | Token verification, activation, refresh, removal queues | none | HeardRight |
| `RightKitDiagnostics` | Bounded JSONL diagnostics/privacy redaction | none | HeardRight project integration |
| `RightKitArtifacts` | Verified staged artifact installation/rollback | none; links system `z` | no app pin confirmed |
| `RightKitPurchases` | StoreKit 2 purchase and entitlement ledger | `RightKitLicense` | HeardRight project integration |
| `RightKitCloudSync` | CloudKit sync engine/transports/retry | none | HeardRight project integration |
| `RightKitStore` | App Group configuration, leases, locking, revisioned state/command queue | none | HeardRight; HeardRight mobile E2E |
| `RightKitImportQueue` | Share extension import queue/payload extraction | `RightKitStore` | HeardRight |
| `RightKitLiveActivity` | Live Activity lifecycle/host | none | HeardRight |
| `RightKitWatchBridge` | Watch delivery ledger/support | none | HeardRight; HeardRight mobile E2E |
| `RightKitSpeech` | Apple Speech, Parakeet/Whisper Core ML, VAD/dictation | none | HeardRight; HeardRight speech E2E |
| `RightKitIntents` | Recording bridge/App Intents seam | none | HeardRight |
| `RightKitAppleCredentials` | Apple credential/keychain backend | none | HeardRight, ScrapeRight native helper |
| `RightKitOverlay` | Overlay state/hit-testing/placement | none | HeardRight |
| `RightKitUI` | Adaptive glass surface | none | HeardRight |

The project attaches many of these products to app targets; exact product links are visible in HeardRight’s project file (for example [`project.pbxproj:2304`](</Volumes/D/claude/heardright/ios-native/HeardRightNative.xcodeproj/project.pbxproj:2304>) and [`project.pbxproj:2315`](</Volumes/D/claude/heardright/ios-native/HeardRightNative.xcodeproj/project.pbxproj:2315>)). ScrapeRight’s standalone helper pins RightKitSwift `0.3.7`; HeardRight E2E packages pin `0.3.2` ([`Package.swift:11`](</Volumes/D/claude/scraperight/macos-native/ScrapeRightAppleCredentials/Package.swift:11>), [`Package.swift:4`](</Volumes/D/claude/heardright/ios-native/Tests/RightKitMobileE2E/Package.swift:4>)).

Per-target direct dependency use-sites, public symbol inventories, unused exports, and transitive totals were not exhaustively audited.

## Ownership approval, contracts, scripts, and bins

- `ownership-approval/` contains Node enrollment/trust preparation, schema, adapters/receipt logic, migration-draft tooling, conformance vectors/tests, plus macOS Swift and Windows C# native helpers. The file inventory shows platform implementations and adapter tests ([`ownership-approval/README.md`](</Volumes/D/claude/rightkit/ownership-approval/README.md>), [`ownership-approval/macos/ownership-approval.swift`](</Volumes/D/claude/rightkit/ownership-approval/macos/ownership-approval.swift>), [`ownership-approval/windows/OwnershipApproval.cs`](</Volumes/D/claude/rightkit/ownership-approval/windows/OwnershipApproval.cs>)).
- Six top-level schemas are present: daemon API, delivery envelope, demo plan, content brief, record event, record session. Fixtures cover valid, invalid, semantic-invalid, and version-rejected examples for those contracts ([`contracts/daemon-api.v1.schema.json`](</Volumes/D/claude/rightkit/contracts/daemon-api.v1.schema.json>), [`contracts/delivery-envelope.v1.schema.json`](</Volumes/D/claude/rightkit/contracts/delivery-envelope.v1.schema.json>), [`contracts/demo-plan.v1.schema.json`](</Volumes/D/claude/rightkit/contracts/demo-plan.v1.schema.json>), [`contracts/content-brief.v1.schema.json`](</Volumes/D/claude/rightkit/contracts/content-brief.v1.schema.json>), [`contracts/record-event.v1.schema.json`](</Volumes/D/claude/rightkit/contracts/record-event.v1.schema.json>), [`contracts/record-session.v1.schema.json`](</Volumes/D/claude/rightkit/contracts/record-session.v1.schema.json>)).
- Scripts found: `generate-index.mjs`, `install-right-ctl.sh`, `run-ui-tests.sh`. No root `bin/` or `templates/` directory was found. Package-owned CLIs are catalogued in [`INDEX.md:61`](</Volumes/D/claude/rightkit/INDEX.md:61>)–[`INDEX.md:87`](</Volumes/D/claude/rightkit/INDEX.md:87>).

## G. Cross-cutting duplicated responsibilities — initial findings

These are candidates for ownership review, not an exhaustive duplicate census.

- **Licensing:** JS `@rightkit/license`, Rust `rightkit-license`, and Swift `RightKitLicense` all participate in licensing, with distinct host/security roles described in the repository docs ([`README.md`](</Volumes/D/claude/rightkit/README.md>), [`swift/RightKitSwift/README.md:8`](</Volumes/D/claude/rightkit/swift/RightKitSwift/README.md:8>)). Keep one wire/token contract and language-specific adapters; do not merge implementations across runtimes.
- **Diagnostics/logging:** npm `@rightkit/logs`, Rust `rightkit-logs`, Swift `RightKitDiagnostics`. Each is a platform-language implementation of a related diagnostics capability ([`INDEX.md:28`](</Volumes/D/claude/rightkit/INDEX.md:28>), [`swift/RightKitSwift/README.md:9`](</Volumes/D/claude/rightkit/swift/RightKitSwift/README.md:9>)). Align schema/redaction/retention contracts; retain native APIs.
- **Process ownership:** Rust `rightkit-process`, Rust `rightkit-sandbox`, JS build-control broker/client process-tree and spawn code. These have overlapping lifecycle/containment surfaces but distinct product contexts ([`INDEX.md:38`](</Volumes/D/claude/rightkit/INDEX.md:38>), [`INDEX.md:43`](</Volumes/D/claude/rightkit/INDEX.md:43>), [`packages/build-control-broker/src/process-tree.mjs:1`](</Volumes/D/claude/rightkit/packages/build-control-broker/src/process-tree.mjs:1>), [`packages/build-control-clients/src/remote-worker.mjs:19`](</Volumes/D/claude/rightkit/packages/build-control-clients/src/remote-worker.mjs:19>)). Define shared lifecycle invariants and avoid attempting cross-runtime code sharing.
- **Atomic writes/locking:** Rust `rightkit-fs`, Swift `RightKitStore`, and JS release/build-control storage routines all implement some durable-file primitives ([`INDEX.md:22`](</Volumes/D/claude/rightkit/INDEX.md:22>), [`swift/RightKitSwift/Package.swift:36`](</Volumes/D/claude/rightkit/swift/RightKitSwift/Package.swift:36>), [`packages/build-control-clients/src/index.mjs:6`](</Volumes/D/claude/rightkit/packages/build-control-clients/src/index.mjs:6>)). Compare semantics and contracts; platform-specific locking should remain native.
- **JSONL framing/logging:** `rightkit-framed-sidecar`, Rust/Swift logging, and JS diagnostics overlap at the format/transport boundary ([`INDEX.md:21`](</Volumes/D/claude/rightkit/INDEX.md:21>), [`INDEX.md:28`](</Volumes/D/claude/rightkit/INDEX.md:28>)). Clarify which crate owns transport framing versus log schema.

## H. Split/merge candidates

- **Build-control npm packages:** core, clients, broker, policy, staging, and phase0 form one private operational system, split into several packages with direct workspace edges. Keep separation where trust/process boundaries require independent modules; evaluate merging policy and core only after a complete import/use graph. Evidence for package relationships is in the package manifests; the README describes build-control as private implementation packages ([`README.md`](</Volumes/D/claude/rightkit/README.md>)).
- **Swift store/import queue:** `RightKitImportQueue` directly depends on `RightKitStore`; this is a coherent module boundary if App Group persistence is reused elsewhere. No merge recommendation without import-volume evidence ([`Package.swift:36`](</Volumes/D/claude/rightkit/swift/RightKitSwift/Package.swift:36>)).
- **Release package:** `@rightkit/release` owns many release, signing, publication, and ownership helpers; its source file inventory is unusually broad. Consider splitting only if a consumer boundary or independent release cycle is established; no split is proven necessary by this read-only pass.

## I. Removal/merge candidates

- `@rightkit/i18n` had no consumer pin in the requested app-manifest search. That is **zero observed consumers in those manifests**, not proof of zero imports or zero external users; inspect package source and all consuming lockfiles before removal.
- `@rightkit/agent-room-mcp` and `@rightkit/build-control-phase0` are private/internal packages. No app consumers were found; their operational/internal consumers were not exhaustively searched, so removal is not recommended from this evidence.
- No crate or Swift target is recommended for removal: consumer and source-wide import graphs were not completed.

## J. pnpm lockfile/workspace

- `pnpm-workspace.yaml` scopes the workspace to `packages/*`; workspace linking is preferred for development. `@wdio/native-utils` is overridden to `2.5.0`; native canvas and browser-driver build scripts are disabled ([`pnpm-workspace.yaml:1`](</Volumes/D/claude/rightkit/pnpm-workspace.yaml:1>)–[`pnpm-workspace.yaml:15`](</Volumes/D/claude/rightkit/pnpm-workspace.yaml:15>)).
- Multiple versions are confirmed for at least the packages listed in “Scope & source-of-truth findings.” The full list and importer-to-version paths were not extracted, so no claim is made that this list is complete.
- The repository’s root `package.json` pins pnpm `11.24.0` and defines package-specific test/build scripts ([`package.json`](</Volumes/D/claude/rightkit/package.json>)).
- Cargo workspace dependencies and Cargo.lock duplicate crate versions were outside this assigned section and were not audited.

## K. Ranked follow-up actions

1. **Reconcile generated index/version data.** Regenerate only through the documented workflow after normal authorization; first compare `INDEX.md` against source manifests and `rightkit-versions.json`. A stale catalog can misstate versions/publication status ([`INDEX.md:5`](</Volumes/D/claude/rightkit/INDEX.md:5>), [`rightkit-versions.json:4`](</Volumes/D/claude/rightkit/packages/release/rightkit-versions.json:4>)).
2. **Complete dependency-use and lock graph audit.** For each npm package, enumerate runtime/peer/optional/dev dependencies, grep full package including build scripts and tests, attribute lockfile transitive counts, and identify duplicates. No dependencies removed yet.
3. **Reconcile app pins to staged npm versions.** App manifests show several older staged versions, including release, app-shell, media-ui, legal, license, platform-ui, onboarding, and theme. Update risk depends on behavioral/API compatibility; do not batch-update without per-package compatibility checks.
4. **Audit lifecycle/storage duplication.** Compare Rust, JS, and Swift process/file persistence semantics before proposing shared contracts or merges. Risk is elevated because containment, atomicity, and platform trust boundaries differ.
5. **Establish consumer evidence for `i18n` and internal packages.** A missing app manifest pin is insufficient evidence for deletion; inspect source imports, scripts, tooling, and lockfiles first.

**No repository files were changed.**