# Family review: Content pipeline & social, Ingest & library, Mail & workspace

Read-only review, 11 Nov 2026. RightKit at `d17fd41`. Paths: `RK/` = `/home/user/rightkit/crates/`, `PK/` = `/home/user/rightkit/packages/`, `S/` = the survey trees.
CodeRight and Legion source are not available, so their feeds (`rightkit-browser`, `rightkit-research`) are not compared.
"Planned lines" are from `coverage-rows.json` (the plan's path-based mapping). "V" = verified by reading, "S" = suspicion.

Findings that apply to the whole family (all V):

- None of the surveyed apps depends on any package in these three families, except HeardRight main on `rightkit-transcript-library =0.1.2` and the older shared crates (`rightkit-browser`, `rightkit-search`, `rightkit-fsindex`). Every other package here is a copy that no app compiles against yet.
- The moved crates carry almost no tests: `#[test]` count is 0 in `rightkit-library`, `rightkit-mail`, `rightkit-google-apis`, `rightkit-social-*`, `rightkit-content-plan`, `rightkit-media-fetch`, `rightkit-web-extract`, `rightkit-web-session`, `rightkit-pipeline`, `rightkit-transcript-library`; 1 in `rightkit-mail-store`. The four UI packages have no test files. The tests stayed in the apps (MailRight `src-tauri/src/db/tests` alone is 5,270 lines).
- The UI packages are whole app front ends placed in a folder, with Tauri command names hard-wired.

---

# A. Ingest & library

## rightkit-library (cargo) — deep

1. Exists: `RK/rightkit-library` 0.1.1, 4,561 lines.
2. Plan feeds: HeardRight 6,049, GenRight 4,596, ScrapeRight 4,280, PostRight 1,836.
3. Today it is ScrapeRight's code only: `scraperight-store` (`lib.rs`), engine `history.rs`, and the enrichment modules. README says so. (V)
4. The four apps do not implement one capability. They implement four:
   - ScrapeRight: an index of extracted artifacts. Plain SQLite table `transcripts` keyed by id, with `url`, `kind`, paths to txt/md/srt/slide_dir, `media_path`, `srt_text`, `truncated_at_s` (`RK/rightkit-library/src/lib.rs:64-125`). Files are the source of truth. Reader functions take `HistorySettings{output_dir, downloads_dir}` and return `Result<_, String>` (`history.rs:631-828`).
   - HeardRight (live): a dictation delivery history. Trait `HistoryStore` over `DeliveryRecord` with drafts, `finalize_session`, retention pruning, origin-scoped update/delete (Mac vs iOS records), in-memory store (`S/heardright-live/tauri-app-next/heardright_core/src/history.rs:103-250`), and an encrypted SQLCipher store with key from the credential store, `secure_delete=ON`, legacy JSON migration, table `delivery_history` (`src-tauri/src/history/sqlcipher_conn.rs:14-39`, `migrations.rs:9`, `sqlcipher_store.rs:77-440`). HeardRight main adds quarantine/recovery (`history/recovery.rs`, 978 lines) and key rotation.
   - GenRight: `genright-gen/src/store.rs` is a job store over `rightkit-job-queue` with an `assets` table and spend ledger; `search.rs`/`import.rs`/`provenance.rs` are asset search and import; `genright-core/src/artifacts.rs` is a gallery index over output folders with brand/eyes/jury filters.
   - PostRight: `engine/src/content.rs` is reusable-media records with cooldowns, expiry, occurrences and A/B variants (`content.rs:1-20`). That is content planning, not a library.
   Where they are in RightKit: HeardRight's trait and records are in `rightkit-transcript-library` (identical apart from one added trait method; see that section). GenRight's store/search/import are in `rightkit-media-gen` (`src/store.rs`, `search.rs`, `library.rs`). PostRight's content records are nowhere in RightKit (`cooldown_days` has no hit in `RK/`).
5. Best shape: keep `rightkit-library` as the artifact index for extraction apps (ScrapeRight today; HeardRight's file transcription and CutRight imports are the plausible second users), and stop treating it as the target for the other three. For that role it needs: a caller-defined kind (string or trait) instead of the fixed social `Kind` enum; no media-fetch dependency; typed errors; the store path passed in, not read from `HistorySettings`. Today's package is not that shape.
6. Per app:
   - HeardRight: wrong. No encryption, no retention, no drafts, no origin scoping, different record. Its package is `rightkit-transcript-library`.
   - ScrapeRight: fits (it is its code), but ScrapeRight does not depend on it yet.
   - GenRight: wrong for jobs/assets (belongs to `rightkit-media-gen`); gaps for the gallery index (present only as a dead file).
   - PostRight: wrong. `content.rs` belongs with `rightkit-content-plan`.
7. Boundary problems (all V):
   - `src/artifacts.rs` (965 lines, GenRight's gallery index) and `src/path_policy.rs` are not declared in `lib.rs` (`lib.rs:37-39` lists only `enrichment`, `history`, `history_types`) and use `serde_json`, which is not a dependency. They never compile, yet `include = ["src/**"]` would publish them.
   - `Kind` is defined twice with the same variants: `rightkit-library/src/history_types.rs:15` and `rightkit-media-fetch/src/protocol.rs:13`.
   - `publish_replacing` is defined twice: `rightkit-library/src/history.rs:25` and `rightkit-media-fetch/src/extraction.rs:144`.
   - The crate depends on `rightkit-media-fetch` (yt-dlp, ffmpeg, scraper) for two URL helpers (`enrichment_source.rs:14,23`).
   - ScrapeRight product rules sit in the code: Free/Pro handling of timed files (`history.rs:1558`, `lib.rs` doc on `srt_text`/`truncated_at_s`), "Lane S-10" comments, the table name `transcripts`, the file name `history.db`.
   - The name collides in meaning with `rightkit-transcript-library`; both export `history::HistoryPage`.
8. Actions:
   - Remove `artifacts.rs` and `path_policy.rs` from this crate; give GenRight's gallery index a home in `rightkit-media-gen` (or a declared, compiled module here with `serde_json`).
   - Replace the fixed `Kind` with a host-supplied kind; delete the duplicate enum; move the two URL helpers to a small shared place and drop the `rightkit-media-fetch` dependency.
   - Keep one `publish_replacing` (in `rightkit-fs`).
   - Move tier rules out (the host decides what to store).
   - Bring ScrapeRight's history tests over (ScrapeRight `engine/src/history.rs` has three test modules, 2,604 lines against 1,846 here).
   - Correct the plan: HeardRight → `rightkit-transcript-library`; GenRight → `rightkit-media-gen`; PostRight `content.rs` → `rightkit-content-plan`.

## rightkit-transcript-library (cargo) — deep

1. Exists: `RK/rightkit-transcript-library` 0.1.2, 1,526 lines (`delivery.rs` 975, `history.rs` 525, `net.rs` 13).
2. Not in the plan as a target. In practice it holds the HeardRight code the plan sent to `rightkit-library`.
3. HeardRight's `heardright_core` `delivery`, `history`, `net`. `history.rs` differs from the live build only by doc comments, the added `rotate_encryption_key` default method, and the removed test module (diff of `S/heardright-live/.../heardright_core/src/history.rs` against `RK/.../history.rs`). (V)
4. Single app. HeardRight main already consumes it (`heardright_core/Cargo.toml:35`). Still in the app: `src-tauri/src/history/` (SQLCipher store 456, connection 223, recovery 978, key rotation 87, legacy JSON migration 148, file store 186) and `history_export.rs` 193. The README states this.
5. Best shape: this crate is the dictation history owner. It should add the SQLCipher store behind a feature, with the key source, quarantine folder and telemetry as parameters, plus export. `delivery.rs` is not library code: paste attribution, target snapshots, terminal handling, leading-space logic (`delivery.rs:302-970`) are dictation delivery and belong with `rightkit-dictation-*`; only `DeliveryRecord` and its enums are the stored record.
6. HeardRight: gaps (the disk store, recovery and export are still app code). Others: n/a.
7. Boundary: `MAC_DELETE_AUTHORITY` and `HistoryOrigin` encode HeardRight's Mac/iOS sync rule (`history.rs:256-267`); acceptable as the reference behaviour but app-specific in naming. `delivery.rs` overlaps `rightkit-dictation-command-host/src/delivery/` (noted in `hr-review-automation-asr.md:78`; not re-read here, S).
8. Actions: add the encrypted store + recovery + export (feature-gated); move delivery logic other than the record types to the dictation family; bring `history_tests.rs` and the store tests; rename or document against `rightkit-library`.

## @rightkit/library-ui (npm) — deep

1. Exists: `PK/library-ui` 0.1.0, about 14,700 lines.
2. Plan feeds: ScrapeRight 8,194, HeardRight 2,870.
3. Both apps' screens copied side by side: `src/queue/**` is ScrapeRight's whole desktop front end (queue board, paste bar, job cards, library, reader, plus its own `lib/theme.ts`, `lib/license.ts`, `lib/updateAuth.ts`, `components/TitleBar.tsx`, `views/settings/*`, `lib/ipc/qaMock.ts` 869 lines); `src/history/**` is HeardRight's History and Transcribe pages. Nothing is shared between the two halves. (V)
4. Side by side:
   - Data: HeardRight lists `HistoryItem` delivery records through `invoke("query_history")`, with retention, per-item edit, multi-delete, export, recovery banner (`src/history/pages/history/HistoryImpl.tsx:145-369`). ScrapeRight lists `HistoryRow` artifacts through `invoke("get_history_page")`, with detail reader, variants, find-in-text, save-as (`src/queue/views/Library.tsx:67-297`, `Detail.tsx:149-157`).
   - Both call Tauri commands by literal name; neither takes a data source prop.
   - HeardRight half imports `@rightkit/dictation-ui` for `PageShell`, `ErrorBanner`, `invoke`, `fieldStore`, `HistoryItem`, and even `pages/history/AppIcon` and `historyUtils` (so the History page is split across two packages: `PK/dictation-ui/src/pages/history/` and `PK/library-ui/src/history/pages/history/`).
   - ScrapeRight half calls sign-in and submit commands (`sign_in`, `submit_playlist`, `submit_profile_images`, `import_auth_cookies`: `PasteBar.tsx:95,118`, `JobStatePreview.tsx:31-71`).
5. Best shape: a list/detail library kit with no IPC of its own: virtualised list, search box, selection and bulk actions, date grouping, detail reader with find, delete/export confirmations, all driven by a `LibrarySource` interface the host implements. HeardRight's History is the reference for list behaviour (search, retention menu, selection, export); ScrapeRight's reader is the reference for detail. Queue UI (paste bar, job cards, queue board) is a separate package (`@rightkit/queue-ui` or part of a job-queue UI). Today's package is not that shape.
6. HeardRight: wrong as it stands. The History default export (`HistoryImpl`) is not even in the root barrel, and adopting it means importing `dictation-ui` internals; nothing is gained over keeping the page. ScrapeRight: fits only in the sense that it is its own front end moved; it brings ScrapeRight's theme, licence and title bar with it.
7. Boundary: package depends on `@rightkit/license`, `app-update`, `settings-ui`, `tauri-shell`, `dictation-ui` (one app's whole shell); file names `ClaudeDesktop.css` (2,719 lines) and `ClaudeReader.css`; five modules are left out of the barrel because of name collisions (README); `Transcribe.tsx` is a file-transcription page, not library.
8. Actions: define the `LibrarySource`/row/detail contract; rebuild list from HeardRight's History and reader from ScrapeRight's Detail on that contract; move queue components out; remove theme/licence/update/title-bar/settings/QA-mock code; put History's `AppIcon` and `historyUtils` in one package; drop the `dictation-ui` dependency.

## RightKitLibrary (swift) — light

1. Does not exist in `swift/RightKitSwift/Sources`.
2. Plan feeds: HeardRight 4,984 (main: about 5,000), ScrapeRight 1,072.
3. Nothing.
4. HeardRight iOS: `TranscriptLibrary` (617), `SharedTranscriptStore` (919, app-group store with tombstones), `TranscriptCloudSync` actor on CloudKit (437), import/export, weekly insights. ScrapeRight iOS: `Library/ArtifactStore.swift` (91), `ArtifactMarkdown.swift` (62) and three SwiftUI views. Same split as on desktop: dictation transcript records with sync versus extracted artifacts. (V by reading type declarations only)
5. Best shape: a `RightKitTranscriptLibrary` module from HeardRight (store, record, tombstones, import/export) on top of the existing `RightKitStore` and `RightKitCloudSync` modules. ScrapeRight's 150 lines of artifact store do not justify a shared module yet.
6. HeardRight: missing. ScrapeRight: missing.
7. Boundary: the mapping sends `ContentViewSettings.swift` (1,025) and `ContentViewSettingsAdvanced.swift` (375) here; they are settings screens. `SharedTranscriptContracts.swift` contains shortcut actions, recording activity state and deep links (`:71-173`), which are not library. `TranscriptCloudSync` overlaps `RightKitCloudSync`.
8. Actions: create the module from HeardRight's store types; route sync through `RightKitCloudSync`; remap settings views to the settings module.

## rightkit-media-fetch (cargo) — deep

1. Exists: `RK/rightkit-media-fetch` 0.1.2, 5,117 lines.
2. Plan feeds: ScrapeRight 8,515, GenRight 1,362.
3. ScrapeRight's `scraperight-audio`, `scraperight-detect`, and engine extraction modules, behind host traits (`AuthLayout`, `PublicWeb`, `BatchContext`, `CarouselEnv`).
4. GenRight's file is `genright-gen/src/stock.rs` (stock media search/download from providers). It is a different capability and already lives in `RK/rightkit-media-gen/src/stock.rs`. No two-app comparison applies.
5. Best shape: three layers. (a) yt-dlp fetch + normalise + `detect` classifier: generic, right as it is (`lib.rs:82-225`). (b) platform scrapers (carousel, TikTok, X, profile/playlist batches): generic behind the traits. (c) job/entitlement wire types: these are ScrapeRight's product and should not be here.
6. ScrapeRight: fits. GenRight: n/a (mis-mapped).
7. Boundary (V): `protocol.rs:185-246` defines `LicenseTier`, `EntitlementsSnapshot`, `FREE_MAX_TRANSCRIPT_SECONDS = 1800`, `FREE_CONCURRENT_DOWNLOADS`, `PRO_CONCURRENT_DOWNLOADS`: ScrapeRight pricing. `JobInfo`, `Stage`, `Kind` in `protocol.rs` are ScrapeRight's queue wire format and duplicate `rightkit-library`'s `Kind`. `extraction.rs:565-850` holds routing rules phrased in ScrapeRight job terms.
8. Actions: move tier/entitlement types to the host (pass limits as plain parameters); one `Kind`; remove the `stock.rs` mapping from the plan; port the tests.

## rightkit-web-extract (cargo) — light

1. Exists: 0.1.2, 1,783 lines.
2. ScrapeRight 2,734 (of which about 650 is Swift: `WebViewRenderer`, `ArticleExtractor`, `CarouselExtractor`).
3. ScrapeRight's article ladder (direct, browser headers, TLS fingerprint, headless browser) over `ArticleNet` and `BrowserHost` traits.
4. Single app.
5. Shape is right: no transport of its own, host traits for network and browser.
6. ScrapeRight: fits. The Swift extractors have no Swift counterpart (missing).
7. Boundary: `tls.rs`/`tls_ffi.rs` load a tls-client shared library by path (`libloading`); a second app inherits that mechanism (S: acceptable since the host supplies trusted paths). `HostPin`/`BrowserConfig` here duplicate concepts in `rightkit-browser` (S).
8. Actions: tests; decide whether the iOS extractors get a Swift module or stay in the app.

## rightkit-web-session (cargo) — deep

1. Exists: 0.1.0, 1,934 lines.
2. Plan feeds: Pulse 2,734, ScrapeRight 1,434.
3. Pulse's `core/src/browser/*` only (README; coverage 688/708 and 359/367 lines).
4. Different capabilities:
   - Pulse: an agent asks the owner for a Chromium session limited to named sites; owner approves on a paired device with a signed challenge; live view frames; one session at a time (`RK/rightkit-web-session/src/lib.rs:400-1015`).
   - ScrapeRight: a Tauri webview loads a platform login page and exports that platform's cookies to `auth/<platform>_cookies.txt`; also import from an installed browser profile (`S/scraperight/src-tauri/src/inapp_sign_in.rs:1-25`, `commands_auth.rs`, `browser.rs:30-312`).
5. Best shape: keep this crate as "owner-approved agent browser session" (Pulse is the reference, rank 3). ScrapeRight's sign-in is cookie capture; its home is `rightkit-browser` (`cookie_export.rs`, `cookies.rs` already write Netscape files fail-closed) plus a small Tauri sign-in window helper in `rightkit-tauri-shell`.
6. Pulse: gaps (it does not depend on the crate yet; doc and paths still say `pulse browser request`, `/etc/pulse/browser.json`: `lib.rs:4,54,315,330`). ScrapeRight: wrong (nothing here serves in-app sign-in).
7. Boundary: depends on `rightkit-remote-gateway`, `rightkit-owner-approvals`, `rightkit-app-host`; error type is `PhoneError`; Pulse CLI names in docs.
8. Actions: remove Pulse names and default paths; remap ScrapeRight files to `rightkit-browser`/`rightkit-tauri-shell`; tests.

## rightkit-browser (cargo, existing 0.3.10) — light

1. Exists, 15,375 lines, used by ScrapeRight, GenRight, Pulse (=0.3.6).
2. Plan: CodeRight 3,460 EXTEND (source not available, not reviewed), ScrapeRight 932 (`browser/managed.rs` 654, `pin.rs` 218).
3. Its own implementation.
4. ScrapeRight's `managed.rs` is a synchronous adapter: one tokio runtime per browser, each call blocks with cancel polling (`managed.rs:1-13`); `pin.rs` builds a network policy from addresses the engine already resolved. This is an adapter from `rightkit-browser` to `rightkit-web-extract::HeadlessBrowser`.
5. Best shape: add a `blocking` module to `rightkit-browser` (or an adapter crate feature in `rightkit-web-extract`) so the sync adapter is not rewritten per app.
6. ScrapeRight: gaps (sync adapter and cookie sign-in helper). CodeRight: not verified.
7. None found.
8. Actions: blocking adapter; host the cookie sign-in flow described under `rightkit-web-session`.

## rightkit-search (cargo, existing 0.4.5) — light

1. Exists, 3,779 lines; used by ScrapeRight, GenRight, MailRight, CutRight.
2. Plan: MailRight 944 EXTEND (`db/fts_index.rs`, `fts_rebuild_job.rs`, `db/fts_text.rs`).
3. Own implementation; `fts.rs` already has external-content FTS5 create/rebuild/drop statements.
4. MailRight's `fts_index.rs` is an online rebuild: fill `fts_messages_next` in batches with a cursor, then swap (`RK/rightkit-mail-store/src/db/fts_index.rs:1-14`). It is in `rightkit-mail-store`, with mail table names.
5. Best shape: the resumable shadow-table rebuild is generic and should be a `rightkit-search::fts` function taking table names; mail-store calls it.
6. MailRight: gaps.
7. Mail-specific copy of a generic procedure in `rightkit-mail-store`.
8. Actions: lift the rebuild-and-swap into `rightkit-search`.

## rightkit-fsindex (cargo, existing 0.6.2) — light

1. Exists, 10,537 lines.
2. Pulse 2,980 ADOPT (`engine/src/disk_index.rs`).
3. Own implementation.
4. Pulse already uses it (`rightkit-fsindex =0.5.0`; `disk_index.rs:275-279`). The 2,980 lines are Pulse's host: one index per volume, state dir, hub log lines, the `hubFileIndex` switch.
5. Shape is right. A per-volume manager with status reporting could move in later (S).
6. Pulse: fits (version bump 0.5.0 → 0.6.2 pending).
7. None.
8. None.

## rightkit-feed (cargo) — light

1. Exists, 0.1.0, 816 lines. Fresh implementation (RSS/Atom parser), not taken from an app.
2. No planned feed, no dependant.
3-8. Bounded, dependency-light, no app decisions. Shape: best. No action besides finding a first consumer (PostRight digests are caller-supplied and do not parse feeds).

## rightkit-catalog (cargo) — light

1. Exists, 0.1.0 unpublished, 449 lines. It is a localisation catalog generator (JSON string catalogs to TS/Rust/Swift), not a media or library catalog.
2. No planned feed.
3. Fresh.
4-8. Not part of this family in substance; belongs with `@rightkit/i18n` / `rightkit-i18n`. Shape: best for its own purpose (S, light read). Action: list it under the i18n family in the matrix.

---

# B. Content pipeline & social

PostRight is the only source for six of these seven packages and was started just before the RightKit decision, so "fits" below means "it is PostRight's code", not "validated by a second app".

## rightkit-pipeline (cargo) — deep

1. Exists, 0.1.2, 3,425 lines.
2. Plan feeds: CutRight 13,753, GenRight 1,701, PostRight 2,521.
3. CutRight's `cutright-genright` crate: a client that drives an installed `genright-engine` (submit, wait, import, journal, recover), plus `channel_recipe.rs` behind a feature.
4. Three different roles around one contract:
   - CutRight → GenRight job orchestration (`orchestrator.rs`, `engine.rs`, `journal.rs`): in the crate.
   - Executors of `brief run`: CutRight `cutright-engine/src/brief_cmd.rs` (maps a content brief to render/QA/captions/thumbnail) and GenRight `genright-engine/src/brief.rs` (maps a brief to image generation). Same CLI and JSON contract by GenRight's own header (`brief.rs:1-12`). Both carry their own `write_json`, `rel`, envelope id and failure-row helpers (`brief_cmd.rs:209-258`, `brief.rs:711-1451`).
   - Caller: PostRight `engine/src/dispatch.rs` runs `<engine> brief run` as an owned child with timeout, output caps, attempt table and sweep (`dispatch.rs:155-190`).
   Not in the crate: any of the brief-run code; CutRight's `genright_cmd.rs` (3,868), `batch_cmd.rs`, `recipe_cmd.rs`, `recipes_cmd.rs` (engine commands).
5. Best shape: two modules. `genright` (today's content, renamed honestly: it is a GenRight engine client; event names are `genright.*`, lock label `genright-journal`: `orchestrator.rs:229`, `journal.rs:159`). `brief_run`: the shared process contract, one runner for callers (spawn, caps, timeout, envelope read and validation; from PostRight's dispatch) and one harness for executors (argument parsing, exit codes 0/1/2, progress lines, envelope write; from CutRight/GenRight). Stores and the product mapping stay in apps.
6. CutRight: fits for the orchestrator; gaps for brief/batch/recipe commands. GenRight: gaps (executor harness absent). PostRight: gaps (runner absent).
7. Boundary: crate name says pipeline, content is one engine's client; `channel_recipe.rs` pulls `rightkit-captions`, `rightkit-color`, `rightkit-audio` and is a render-preset check that belongs with compose/render; `examples/live_step3.rs` is CutRight's dev example.
8. Actions: add the brief-run runner and executor harness; decide name (`rightkit-pipeline` with `genright` module, or rename); move `channel_recipe` out; tests (CutRight `tests/orchestration.rs` 1,184 lines and the fake engine binary were not brought).

## rightkit-social-publish (cargo) — deep

1. Exists, 0.1.3, 13,982 lines.
2. PostRight 16,930.
3. PostRight's publish engine: ten platform adapters, the phase machine, quota, accounts, deletion, refresh, with host-implemented store traits (`PublishJobStore`, `PublishQuotaStore`, `MachineStore`, `AccountStore`, `DeletionStore`).
4. Single app.
5. Shape: adapters + one-request HTTP layer + a restartable machine over store traits is the right design (`machine.rs:1-12`, `:106-129`). The SQL store that implements the traits is not in RightKit (PostRight `publish/mod.rs` 991 lines, 48 found), so a second app must write its own tables.
6. PostRight: gaps (store implementation and method dispatch remain in the app).
7. Boundary: `Scope{venture, channel, account}` (`protocol/mod.rs:15`) is PostRight's tenancy model and is threaded through every trait; `calendar_time.rs` (time zones) is not publish-specific and `content-plan` depends on this crate partly for it (S); README still describes only the adapters, while `lib.rs` lists the machine, quota, accounts, deletion (V).
8. Actions: ship a reference SQLite store (feature) or document the schema the traits expect; make `Scope` an opaque key or generic; move `calendar_time` to `rightkit-time`; update README; port tests.

## rightkit-social-analytics (cargo) — deep (two feeders)

1. Exists, 0.1.2, 5,136 lines.
2. PostRight 5,186; CutRight 1,300.
3. PostRight's `links`, `analytics`, `metric_import`, `optimise` over store traits; plus CutRight's `retention_cmd.rs` as `cmd/retention.rs` behind feature `engine-commands`.
4. The two do not overlap. PostRight: platform insight pulls, tracked links, CSV metric import, calendar change proposals. CutRight: an audience-retention CSV bound to one exact render, mapped through a time map to timeline cuts and applied as a timeline revision (`cmd/retention.rs:1-21`); schema ids are `cutright.retention.v1` and `cutright.render-request.v1` (`:35-36`).
5. Best shape: PostRight's four modules stay. CutRight's retention is video editing driven by a metric file; it belongs with `rightkit-autoedit`/`rightkit-video-project`. The shared piece would be one retention-CSV parser used by both `metric_import` and retention (S: not compared line by line).
6. PostRight: gaps (stores in app). CutRight: wrong home (works, but drags `rightkit-engine-host`, `rightkit-video-project`, `rightkit-timeline`, `rightkit-transcript`, `rightkit-autoedit` into a social crate).
7. Boundary: as above; README describes only link rules (stale); depends on `rightkit-social-publish` for `Scope`/`Fail`.
8. Actions: move `cmd/retention.rs` to the video/autoedit family; refresh README; shared small `social-core` (Scope, Fail, calendar time) so the three social crates do not depend on publish for types.

## rightkit-content-plan (cargo) — light-to-deep

1. Exists, 0.1.2, 3,610 lines.
2. PostRight 4,599.
3. PostRight `calendar`, `variants`, `hook_test`, `drafts` over store traits.
4. Single app.
5. Shape is consistent with the other social crates. PostRight's `content.rs` (reusable media, cooldowns, occurrences, variants; 1,513 + 323 lines, mapped to `rightkit-library`) belongs here and is absent.
6. PostRight: gaps.
7. Depends on both `rightkit-social-analytics` and `rightkit-social-publish`; README describes only the draft approval rules (stale).
8. Actions: add content records; README; tests.

## rightkit-content-proof (cargo) — light

1. Exists, 0.1.1, 172 lines (`numbers.rs`).
2. PostRight 3,820.
3. Only the number canonicaliser from `proof.rs`.
4. Single app. `proof.rs` (1,103: claim review, 13-step match, review epoch) and `acquisition.rs` (2,185: approval-gated acquisition requests) are not in RightKit. `acquisition.rs` is not proof; it is an approval gate for ScrapeRight runs.
5. Shape: missing in substance. The claim-review matcher over a store trait is the package; acquisition belongs with approvals or stays in the app.
6. PostRight: gaps (large).
7. `acquisition.rs` mis-mapped.
8. Actions: move claim review; remap acquisition; or leave as the number helper and say so in the matrix.

## rightkit-social-inbox (cargo) — light

1. Exists, 0.1.2, 2,297 lines.
2. PostRight 3,070.
3. PostRight `inbox` (webhook verification, Meta delivery parsing), `automation` (keyword replies) over store traits.
4. Single app.
5. Right design. Meta-only parsing today.
6. PostRight: gaps (stores).
7. Depends on publish and analytics for shared types; README stale.
8. Actions: shared types crate; README; tests.

## @rightkit/social-ui (npm) — light

1. Exists, 0.1.0, 17,421 lines.
2. PostRight 17,034 (coverage 9,840/10,039: a near-complete copy).
3. PostRight's front end: 20 screens, `data/engine` (protocol, mapping, data source) and `data/mock` (19 fixture files).
4. Single app.
5. Shape: it has a `DataSource` seam (`data/source.ts`, engine and mock implementations), which is better than the other UI packages. It is still an entire app, including mock fixtures shipped in the package and "PostRight" in 20 files.
6. PostRight: fits.
7. Mock fixtures and product name in the package; screens for features with no RightKit backend (briefs, digests, text gate, channel memory).
8. Actions: move `data/mock` to a dev entry point; product name through `hostAppName`; low priority until PostRight's status is decided.

## rightkit-research (cargo) — light

1. Exists, 0.1.2, 7,930 lines. Moved from `legion-research` (README).
2. Plan: PostRight 1,572 (`digest.rs`, `reddit_reader.rs`, protocol), Legion (source unavailable).
3. Legion's research pipeline.
4. PostRight's digest is "store dated findings the caller supplies and turn them into draft calendar entries" (`digest.rs:1-8`); it shares only the word research. Coverage 19/996 lines.
5. PostRight digests belong in `rightkit-content-plan` (they create calendar drafts); the Reddit reader is a source reader that could sit in `rightkit-web-extract` or stay in the app.
6. PostRight: wrong. Legion: not verified.
7. Uses `reqwest` directly rather than `rightkit-http` (V from Cargo.toml); `wf_port/mod.rs` is one line.
8. Actions: remap PostRight files; `rightkit-http`; otherwise not verified.

---

# C. Mail & workspace

MailRight is the only feeder. All four Rust crates are MailRight code moved on 1 Nov; MailRight does not depend on any of them yet.

## rightkit-mail (cargo) — deep

1. Exists, 0.1.2, 2,732 lines: `search/*` (query compiler, predicates, planner, paging), `fusion`, `trust`, `images`. No database, no network.
2. MailRight 27,603 planned.
3. MailRight code.
4. Single app. Why only about 39% was found (5,562 of 16,152 distinctive lines): the plan's one target became two crates, and three blocks were not moved at all:
   - Moved to `rightkit-mail-store`: `db/*`, `db_schema.rs`, `db_sync_apply*`, `attach_cache` (found, counted).
   - Not moved, `engine_api/*`: 26 files, 7,541 lines, 91 found. This is the engine facade: a `"group.method"` JSON method table (`S/mailright/src-tauri/src/engine_api/mod.rs:1-14`) with drafts, bulk, mutations, attachments, accounts, settings, maintenance, status, remote. It reads `crate::AppState`.
   - Not moved, tests: `db/tests/*` 27 files, 5,270 lines, 46 found, plus `engine_api/*_tests.rs`.
   - Not moved, sync drivers: `sync/outbox.rs` 347, `sync/backfill.rs` 318 (8 found); `webimg.rs` 307 (the image proxy; only dimension and tracker checks moved); `db/data_dir.rs` 268.
5. Best shape: three crates. `rightkit-mail` (pure rules, as today) is right. `rightkit-mail-store` (see next). A third, `rightkit-mail-engine`, for the method table and the sync/outbox/backfill drivers over a `MailRemote` trait (MailRight already has `MailRemote`/`LiveRemote` in `engine_api/remote.rs`), so the app shell is Tauri glue only. Without the third crate the majority of MailRight's mail code stays in the app.
6. MailRight: fits for what is there; the family as a whole has gaps.
7. Boundary: `search/store.rs`, `thread_sql.rs`, `candidates.rs` emit SQL against mail-store's table names, so the "no database" crate knows the schema (V: file names and README; SQL text not audited line by line, S on extent). The plan's description "provider-neutral local mail mirror" is not what exists (see mail-store).
8. Actions: move the tests for search/trust/images; decide and create the engine crate; state in the plan that the target is split in three.

## rightkit-mail-store (cargo) — deep

1. Exists, 0.1.0, 7,332 lines. Not a plan target (split out of `rightkit-mail`).
2. Feed: MailRight (part of the 27,603).
3. MailRight `db/*`, schema v1 to current, migration gate with backup, outbox and pending ops, sync apply, semantic index (sqlite-vec + embedder).
4. Single app.
5. Shape: one encrypted store with a host trait for identity, key, data folder and build channel (`host.rs:44-73`) is right. Three things are not:
   - The host is a process global (`static HOST: OnceLock`, `host.rs:75-87`); a second store or a test with a different host in the same process is impossible, and using the store before `set_host` panics.
   - It is Gmail-shaped: `db.rs:50` imports `rightkit_google_apis::gmail::{FetchedMessage, LabelInfo}` and `db_sync_apply.rs:272-289` takes `gmail::SyncWork`. A second provider (IMAP, Graph) cannot feed it without producing Gmail types.
   - It carries MailRight features in the schema: agent gate/send/forward migrations v16-v18 (`db_schema.rs:200-274`), MailRight-managed snooze (`:479`), `MAILRIGHT_EMBED_MODEL_DIR` (`db/semantic/embedder.rs:77`).
6. MailRight: fits (schema is its own; adoption needs the `MailStoreHost` impl).
7. Boundary: depends on `rightkit-embed`, `rightkit-onnx`, `rightkit-models`, `sqlite-vec` unconditionally (semantic index should be a feature); `fts_index.rs` rebuild is generic (see `rightkit-search`); agent tables belong to the agent-gateway family.
8. Actions: host passed at open, not global; store-owned message/label input types with a `From` in `rightkit-google-apis` (reverse the dependency); `semantic` feature; move agent migrations behind a registration hook or document them as part of the schema; port `db/tests`.

## rightkit-google-apis (cargo) — deep

1. Exists, 0.1.1, 5,725 lines.
2. MailRight 8,778.
3. MailRight's Google clients: Gmail, Calendar, Drive, People, Tasks, Meet, on one retrying client, plus desktop OAuth.
4. Single app.
5. Shape: one shared `Google` client with per-API modules is right. Not moved: `engine_api/workspace.rs` (385) and `engine_api/people.rs` (277), the method groups that call these clients; and all tests (`gmail/batch_fetch/tests.rs` 401, `calendar/tests.rs` 350, `gmail/tests.rs` 246).
6. MailRight: fits.
7. Boundary: `oauth.rs:19-44` fixes one scope set (`SCOPE`: gmail.modify, full Drive, contacts, calendar, tasks, meet). That is MailRight's Cloud project consent screen; an app wanting only Drive or Calendar inherits full-mailbox scopes. Google sign-in now exists in three places: `rightkit-oauth::google`, `rightkit-google-apis::oauth`, `rightkit-google-identity`.
8. Actions: scopes as a parameter with per-API constants; cargo features per API (`gmail`, `calendar`, `drive`, `people`, `tasks`, `meet`); tests.

## rightkit-google-identity (cargo) — light

1. Exists, 0.2.1, 664 lines.
2. No planned feed; no crate depends on it.
3. Fresh implementation (protocol-only Sign in with Google for the own-cloud identity decision): PKCE, auth request, token bodies, claim checks, RS256 JWS verification, refresh-token store.
4. Overlap with `rightkit-oauth` (`pkce.rs`, `google.rs` `GoogleIdVerifier`, `Jwks`) which `rightkit-google-apis` already uses. README says its endpoint URLs and issuer list are unverified.
5. Best shape: one Google sign-in owner. Either this crate becomes a thin layer over `rightkit-oauth::google` (keeping `RefreshTokenStore` and the Android `IdTokenProvider` seam) or it is merged into `rightkit-oauth`.
6. All apps: n/a today.
7. Duplicate PKCE and ID-token verification with `rightkit-oauth`.
8. Actions: merge or re-base on `rightkit-oauth`; verify endpoints.

## @rightkit/mail-ui (npm) — deep

1. Exists, 0.1.0, 19,633 lines.
2. MailRight 17,055 (8,390 of 9,467 distinctive lines present).
3. MailRight's front end: inbox, reader, compose, sidebar, the app store (`store/*`), `lib/ipc/*` (158 distinct Tauri command names), and QA mocks (`qaMock*.ts`, eight files).
4. Single app.
5. Best shape: views driven by a `MailClient` interface, with the Tauri binding as one implementation in a subpath, so the IPC names are not the package API. The store can stay as the package's state layer.
6. MailRight: fits (its own UI).
7. Boundary: depends on `@rightkit/license`, `onboarding`, `legal`, `legal-ui`, `logs` (app shell concerns); `lib/ipc/pro.ts`, `agents.ts`, `agentApprovals.ts`, `workspace.ts`, `people.ts` put licence, agent and Workspace calls inside the mail package; "MailRight" appears in 12 source files; QA mock data ships in `src`.
8. Actions: `MailClient` seam; move workspace/people/agent IPC out; mocks to a dev entry; product name via `hostAppName`.

## @rightkit/workspace-ui (npm) — light

1. Exists, 0.1.0, 2,213 lines: Calendar, Drive, Contacts, Tasks, Meet views.
2. MailRight 2,722.
3. MailRight's views.
4. Single app.
5. Shape is wrong in one respect: every view imports `@rightkit/mail-ui` internals: `store/index` (`useAppStore`), `hooks/useDrive`, `lib/ipc`, `components/Icons`, `components/AccountSwitcher`, `lib/mail` (`DriveFileMenu.tsx:3-5`, `ContactsView.tsx:8-10`). `useDrive`, `CalendarNav`, `DriveNav` were mapped here but sit in mail-ui (coverage tgt 23/111, 11/85, 6/47). A calendar or Drive screen cannot be used without the mail app's store.
6. MailRight: fits. Any other app: wrong.
7. Circular ownership with mail-ui.
8. Actions: give workspace-ui its own client interface and state slices; move Drive/Calendar hooks, nav and IPC from mail-ui here; share icons and account switcher through a lower package.

---

# Family summary

Counts by shape (26 packages): best 4 (`rightkit-web-extract`, `rightkit-fsindex`, `rightkit-feed`, `rightkit-catalog`); gaps 16; wrong 5 (`rightkit-library`, `@rightkit/library-ui`, `rightkit-web-session` as a two-app target is counted gaps; the five are `rightkit-library`, `@rightkit/library-ui`, `@rightkit/workspace-ui`, `rightkit-social-analytics`, `rightkit-google-identity`); missing 1 (`RightKitLibrary`).

Main points:

1. `rightkit-library` is not one package. The plan merged four capabilities under one name. Only ScrapeRight's is in the crate. HeardRight's history (the live reference) is in `rightkit-transcript-library`, and HeardRight main already uses it; its encrypted store, recovery and export are still app code. GenRight's is in `rightkit-media-gen`, with a dead uncompiled copy of `artifacts.rs` in `rightkit-library`. PostRight's `content.rs` is content planning and is nowhere.
2. `@rightkit/library-ui` is two app front ends in two folders with no shared component, hard-wired Tauri commands and one app's theme/licence/title bar. HeardRight's History page is split between it and `dictation-ui`.
3. `rightkit-mail`'s 39%: the target was split into `rightkit-mail` + `rightkit-mail-store` (both found), and the engine method table (7,541 lines), the sync/outbox/backfill drivers, the image proxy and all tests (5,270+) were not moved. A third crate (`rightkit-mail-engine`) is needed for MailRight to become a thin shell.
4. `rightkit-mail-store` depends on Gmail wire types and a process-global host; the plan called it provider-neutral.
5. Duplicates: `Kind` and `publish_replacing` in `rightkit-library` and `rightkit-media-fetch`; Google sign-in in `rightkit-oauth`, `rightkit-google-apis`, `rightkit-google-identity`; FTS shadow rebuild in mail-store versus `rightkit-search`.
6. App product decisions inside packages: ScrapeRight Free/Pro limits in `rightkit-media-fetch/src/protocol.rs`; MailRight's full OAuth scope set in `rightkit-google-apis/src/oauth.rs`; Pulse names in `rightkit-web-session`; CutRight retention editing inside `rightkit-social-analytics`; PostRight's venture/channel/account scope in every social trait.
7. Mis-mapped by the plan: ScrapeRight sign-in → `rightkit-web-session` (should be `rightkit-browser`); GenRight `stock.rs` → `rightkit-media-fetch` (is in `rightkit-media-gen`); PostRight digest → `rightkit-research`; PostRight `acquisition.rs` → `rightkit-content-proof`; HeardRight settings Swift views → `RightKitLibrary`.
8. Tests were not moved with the code in any crate of the family; READMEs of the social crates describe an earlier, smaller scope than the code.
