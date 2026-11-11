# rightkit-library and the Swift mapping: redraw note

Date: 2026-11-11. Branch `claude/busy-hypatia-e30h7l`. Rows: `rightkit-library`, `RightKitHubQA`, `RightKitUI`, `RightKitWatch` (`../matrix.md`). Taken on the recommended option of the shape review, pending the owner's confirmation. Nothing here is on `main`.

## A. `rightkit-library` (crate, 0.1.1 to 0.2.0)

The review (`family-content-ingest-mail.md`, `rightkit-library` section) found that the plan merged four capabilities under one name, and that the crate held only ScrapeRight's artifact index, with a fixed social `Kind` enum, ScrapeRight's Free/Pro rules and two dead GenRight files.

What changed:

| Item | Before | After |
|---|---|---|
| Kind | `history_types::Kind`, a copy of `rightkit_media_fetch::protocol::Kind` | deleted; `HistoryRow::kind` and `TranscriptDetail::kind` are the host's tag (`String`). The JSON is unchanged for every known tag; an unknown tag reads back as written instead of `"unknown"`. `rightkit-media-fetch` keeps the one enum |
| Short code | the library decided which kinds are Instagram and parsed the code through media-fetch | `EnrichmentInput::shortcode`, filled by the host |
| Domain | `rightkit_media_fetch::detect::host` | the crate's own link normaliser (http(s) host without `www.`, credentials or port). A local file name such as `clip.mp4` is no longer a domain |
| Dependency | `rightkit-media-fetch` (yt-dlp, ffmpeg, scraper) for two URL helpers | dropped |
| Atomic replace | `publish_replacing` in `rightkit-library`, `rightkit-media-fetch` `extraction.rs` and a private copy in `carousel.rs` | one `rightkit_fs::publish_replacing` (rightkit-fs 0.4.13, unpublished, additive); media-fetch re-exports it under its old path |
| Tier rules | Free/Pro wording on `srt_text`, `truncated_at_s` and the detail reader | host-neutral; which records get timed files and whether a transcript is capped is the host's rule. No behaviour change |
| `artifacts.rs`, `path_policy.rs` | GenRight's gallery index, undeclared and never compiled, shipped in the 0.1.x archives | moved, compiled, to `rightkit_media_gen::gallery` (0.1.2). Its copies of media-gen names now use the originals (`host::CALLER_SIDECAR_SUFFIX`, `AssetFacts::unknown()`, the library's `pair.json`, the new `provenance::sibling_facts_path`). The eyes hash and approval-record rule, which `rightkit-media-gen-approvals` and the gallery each had, are now `host::approval::eyes_file_hash` and `eyes_record_bound` in media-gen; `rightkit-media-gen-approvals` 0.1.0 (unpublished) calls them |

Versions: `rightkit-library` 0.2.0 (breaking), `rightkit-media-fetch` 0.1.3, `rightkit-media-gen` 0.1.2, `rightkit-fs` 0.4.13 (unpublished, gains one function), `rightkit-media-gen-approvals` 0.1.0 (unpublished, requires media-gen 0.1.2). No crate in the workspace depends on `rightkit-library` or `rightkit-media-fetch`.

Recipes PK102 (`rightkit-library`), PK105 (`rightkit-media-fetch`), `rightkit-media-gen` and `rightkit-media-gen-approvals` pin the new versions and check the new shape: host kind and short code kept, no domain for a file name, an unknown kind tag returned unchanged by a history page; `publish_replacing` through media-fetch; the gallery listing, caller filter, unknown facts and eyes hash. All four were run against path dependencies in a scratch copy.

Still open (verdict `gaps`): typed errors; the store path and table/file names from the host instead of `HistorySettings`; ScrapeRight to depend on the crate (it keeps its own copy and its `Kind` and tier rules); ScrapeRight's history tests as an end-to-end recipe; the name overlap with `rightkit-transcript-library` (both export `history::HistoryPage`); publish 0.2.0.

### Plan mapping

The master plan and the per-app tables sent four apps' code to `rightkit-library`. Corrected rows (`../../2026-11-01-rightkit-package-master-plan.md`, `per-app/*-rightkit-mapping.md`, `rules/*.tsv`, `packages.json`):

| App | Lines | Was | Now | Why |
|---|---:|---|---|---|
| HeardRight | 3,676 | `rightkit-library` NEW | `rightkit-transcript-library` EXTEND | encrypted dictation history with drafts, retention and origin scoping; the trait and records are already there |
| GenRight | 4,596 | `rightkit-library` NEW | `rightkit-media-gen` ADOPT | job/asset store, search, import and library are in media-gen; the gallery index is `gallery` since 0.1.2 |
| PostRight | 1,836 | `rightkit-library` NEW | `rightkit-content-plan` NEW | reusable media records with cooldowns and expiry are content planning |
| ScrapeRight | 4,280 | `rightkit-library` NEW | unchanged | it is the crate's own code |

## B. Swift mapping (no Swift code changed)

The per-file inventory JSON lives in each app repo (beside `docs/rightkit-inventory/<app>-rightkit-mapping.md` there, for example `heardright-rightkit-files.json`), not in this repo, and was not changed. What this repo holds and what was corrected: the per-app mapping tables and their totals, the path rules that produced them (`rules/_shellpass.tsv`, `heardright.tsv`, `pulse.tsv`, `coderight.tsv`, `scraperight.tsv`), the master plan's rows and coverage table, and the plan's aggregate `packages.json`. These now agree with each other.

| Module | Was planned | Now | Verdict | Why |
|---|---|---|---|---|
| `RightKitHubQA` | HeardRight iOS tests (the inventory JSON; the mapping table already said APPTEST), CodeRight and ScrapeRight iOS tests (rules), Pulse's Notch view snapshots 1,254 | all app tests stay with the app (APPTEST); only Pulse's Hub fixtures, renderer and AX tour (2,277) remain planned here | `gaps` | it holds the right code (Pulse's three Hub fakes, identical); Pulse's Hub QA tooling is still to move in and Pulse still builds from its local copy |
| `RightKitWatch` | HeardRight's 19 Apple Watch files, 3,476 | nothing. `RightKitWatchBridge` EXTEND: watch transfer, phone inbox, delivery (3 files, 972); `RightKitAudio` ADOPT: the meter (1, 27); `RightKitWatchRecording`, a planned module that does not exist yet: controller on `AudioFileRecorder`, processor, recovery, pending store and policy, state, mode, telemetry (11, 1,787); the app: watch app entry and the two `AppIntent` declarations (3, 517, SHELL) and the smoke tests (1, 173, APPTEST) | `best` | a macOS FSEvents file watcher that holds exactly that; no app maps code into it after the fix |
| `RightKitUI` | HeardRight scaffold 3,903 and onboarding 1,563, Pulse Notch settings 2,745 and iOS entry 845, CodeRight iOS entry, File Provider and widgets 835, ScrapeRight settings 422 and entry 288 | nothing: all of it is app code (SHELL). Pulse's `Preferences` model goes to generated settings on `RightKitStore` once those exist | `gaps` | the glass surface and contrast are right and in use by HeardRight; the contrast maths exists in three languages (generate from `rightkit-theme`) |

The row totals of each corrected per-app table are recomputed from its rows; files and lines per app are unchanged. The matrix's planned-lines cells for these modules still come from the app repos' inventory JSON, which keeps the old targets until it is regenerated there with the corrected rules.

## Checks run

`cargo +1.99.1` clippy `--all-features -D warnings`, `cargo doc --no-deps --all-features` with `-D warnings` and rustfmt `--check` on `rightkit-fs`, `rightkit-library`, `rightkit-media-fetch`, `rightkit-media-gen`, `rightkit-media-gen-approvals`; `cargo check` on the direct dependents (`rightkit-media-pipeline`, `rightkit-prompt-recipes`, `rightkit-remote-compute`); `cargo metadata --locked`; the four recipes against path dependencies; `node scripts/preflight.mjs --worktree`.
