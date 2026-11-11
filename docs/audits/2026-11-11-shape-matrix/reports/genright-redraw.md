# GenRight packages: redraw note

Date: 2026-11-11. Branch `claude/busy-hypatia-e30h7l` (work branch `redraw-genright`). Rows: `rightkit-media-gen`, `@rightkit/media-gen-ui`, `rightkit-prompt-recipes`, `rightkit-remote-compute` (`../matrix.md`, all `wrong`), and the new `rightkit-media-gen-models`. Taken on the recommended option of the shape review, pending the owner's confirmation. Nothing here is on `main`.

GenRight is a to-be-decided app. Every GenRight pin in the matrix reads `unused`: the app still builds from its own `genright-*` crates and `src/`, so nothing in the app breaks when a product piece leaves a package. The goal for each package is that a second app (CutRight's generate panel, PostRight's brief dispatch) can adopt it without inheriting GenRight's product decisions, and that no code exists twice in RightKit.

## Shape after the redraw

| Package | Before | After |
|---|---|---|
| `rightkit-media-gen` 0.2.0 | GenRight engine with private copies of `rightkit-ffmpeg` code, a JSON-only API, organisation and app names in paths, tables and token domain | Same engine without the copies; a typed API beside the JSON commands; names are the host's |
| `rightkit-prompt-recipes` 0.2.0 | Every module a `studio.*` handler on `rightkit_media_gen::Context`; owner brand list, brand defaults and venture list; the KDP workflow | Pure library over the embedded data by default (no media-gen in the build); brands are host-supplied; handlers behind feature `commands`; KDP left with the app |
| `rightkit-remote-compute` 0.2.0 | Rented workers plus half a crate of local model management; worker records hard-wired to media-gen's settings table | Rented workers only, their commands, and a `WorkerStore` trait (default: the settings table) |
| `rightkit-media-gen-models` 0.1.0 (new, PK240) | (was `remote-compute/models`) | The engine's local model management (`studio.models.*`: catalogue join, installs, governor, routes) |
| `@rightkit/media-gen-ui` 0.2.0 | The GenRight app frontend with a 4,003-line browser mock, legal documents, onboarding, licence and update wiring, brand data, its own mask painter | Media-generation panels over a host-supplied client (`setMediaGenClient`); host-supplied brands; masks on `@rightkit/media-ui` |

## Duplicates deleted

| Copy | Owner it now uses |
|---|---|
| `rightkit-media-gen/src/media.rs` (477 lines, GenRight's container probe) | `rightkit_ffmpeg::quickprobe` (the extended copy: size, codec, frame count) |
| `rightkit-media-gen/src/host/h264.rs` (135 lines; 19 differing lines, all path prefixes) | `rightkit_ffmpeg::h264_rule`; lip-sync and `rightkit-media-pipeline` (compose, motion, capabilities) call it |
| `rightkit-remote-compute/data/local-image-profiles.json` (byte-identical) | `rightkit_local_gen::profiles::CATALOGUE_JSON` (made public, local-gen 0.1.2) |
| Five read/write-a-JSON-setting helpers (remote-compute `get_json`/`set_json`, governor `load_gov`/`save_gov`, install `load_installs`/`save_installs`, avatars `selected_route`) | `rightkit_media_gen::store::setting_json` / `set_setting_json` |
| `rightkit-prompt-recipes` `brands` (brand catalogue and `infer_brand_code`) | `rightkit_media_gen::gallery` already holds the host-registered brand catalogue and inference; the recipes crate keeps only the brand codes and per-library defaults it composes with |
| `@rightkit/media-gen-ui` `components/MaskBrush` and the painting half of `lib/mask-export` | `@rightkit/media-ui` `MaskPainter` with the new `invert` export option (`components/MaskEditor` is the 70-line step around it) |
| `@rightkit/media-gen-ui/src/studio-data/` (brands, production templates, recipe catalogue graphs, video template libraries) | The engine: `rightkit-prompt-recipes` data through its commands; `studio.recipes.catalogue` now reports each node's `depends_on`, which the UI had read from its embedded copies |
| `@rightkit/media-gen-ui` browser mock (4,003 lines) and five panel mocks | None in the package: a host passes its own `MediaGenClient`; GenRight keeps its mock in its own repo |

## A. `rightkit-media-gen` 0.2.0

- `media` and `host::h264` deleted (above).
- `typed`: `GenerateRequest` (kind, provider, model, prompt, params, request id, upload consent, scope), `MediaGen::{estimate, submit, run, status, wait, result, cancel, ack}`, `Submitted`, `JobRecord`, `Waited`, `JobResult`, `Asset`. It calls the same functions as the JSON commands, so a Rust host (the matrix names CutRight and PostRight, which today shell out to the engine binary) gets the same validation, routing, reservation and descriptor.
- Names: the approval token domain is `host::approval::set_token_domain` (default `rightkit-approval-v1`; GenRight sets `genright-approval-v1`); the ledger sidecar tables are `media_gen_spend_*`, with an in-place rename of `genright_spend_*` on attach; `providers::orthic_root` is `tools_root` and its temp fallback uses the host's `machine_dir_name`.
- `gallery` and `host::approval` (added on the branch) are kept as they are.

Not done, and why:
- **Spend layer onto `rightkit-spend`.** `ledger.rs` (2,278 lines) reserves through `Ledger::reserve_on` already, but implements the three-cap reserve, device-allowance epochs, invoice reconciliation and caller budgets itself, beside `reserve_multi`, `set_device_allowance`, `reconcile_invoice` and `Budget`. Moving them changes how existing databases record periods, epochs and invoices; that needs a migration and a real-data qualification (the owner's rule: real data, snapshot and restore). It is the largest remaining duplicate.
- **Provider adapters split from the engine core.** The adapters use `Context`, the ledger, the store and the credential layer directly; a split is clean only once the typed core is the boundary. Next step after the spend layer.
- **`voices` to `rightkit-tts`, local providers to `rightkit-local-gen`.** Both need the provider trait in a crate below media-gen first. `qts/` is a host-layout module over `rightkit-tts` (re-exports plus model and ONNX Runtime locations), not a copy.

## B. `rightkit-prompt-recipes` 0.2.0

- Default build: contracts and validator, `recipes::compose`/`compose_edit`/`libraries`, the image workflow catalogue, character and LoRA data, video shot-contract templates. Dependencies: `serde`, `serde_json`, `regex`, `time`, `rightkit-time`. No engine, database or network; `rightkit-fs` and `rightkit-logs` come with `commands` (drafts on disk, draft ids).
- Feature `commands`: every `studio.*` handler, `commands()`, `dispatch()`, and the two modules that join the engine's model registry (`recipe_catalogue`, `swap_templates`). Errors are `RecipeError` with conversions both ways to `rightkit_media_gen::GenError`; `dispatch` keeps the engine's result type, so an engine changes nothing but the feature flag.
- Brands: `brands::set_brands(Brands { codes, defaults })`. The owner's `_brand_defaults` were removed from 13 data files, the brand list from the image intents, the venture list (`dd, rh, ts, hr, kdp, dv`) from the video templates, and owner brand names from three video template libraries.
- KDP (`kdp.rs`, `data/kdp`) deleted; it stays in GenRight's `genright-studio`.

## C. `rightkit-remote-compute` 0.2.0 and `rightkit-media-gen-models` 0.1.0

- `models/` moved with `git mv` to the new crate. The matrix suggested `rightkit-models` or `rightkit-local-gen`; neither fits: the module is the engine's command surface over a `rightkit_media_gen::Context` (installs, grants and routes live in the engine's settings table), and both of those crates sit below media-gen. Hardware detection, installs and the store are already `rightkit-models`'; the profile list is `rightkit-local-gen`'s. Precedent: `rightkit-media-gen-approvals`.
- The worker actions moved the other way, into `rightkit_remote_compute::commands` (same `studio.models.*` names). A host routes the five worker commands to remote-compute and the rest to media-gen-models.
- `WorkerStore` with `SettingsWorkerStore` as the default; `set_worker_store` for a host that keeps workers elsewhere.
- Not done: the rental bracket still reserves through `rightkit_media_gen::ledger::reserve`. That is deliberate: a rental must count against the same daily, monthly and device limits as generation, and those limits live in media-gen's ledger. It moves to `rightkit-spend` together with the ledger (A).

## D. `@rightkit/media-gen-ui` 0.2.0

- Deleted from the package (to the app): `App`, `main`, `DesktopWorkspace`, `Settings`, `ComingSoon`, `Onboarding` and its libraries, `LegalDocument`, `legal/documents`, `LicensePlan`, `lib/license`, `lib/app-update*`, theme and window wiring, `brand.css`, `shell.css`, the KDP view and its model and mock, every mock, `studio-data/`, `MaskBrush`. Dependencies on `@rightkit/app-update`, `legal`, `legal-ui`, `license`, `theme` and the Tauri process and updater plugins removed; those packages are what the host's shell uses.
- `lib/client.ts`: `MediaGenClient` (`request`, `approveDraft`, `assetUrl`, `isDesktop`) and `setMediaGenClient`; `tauriClient` is the default. Every `engine_request` call and every mock fallback in `api`, settings, jobs, voices, production and the three studio panels now goes through it.
- `setHostBrands` replaces the shipped brand JSON; `Brand` and `Venture` are strings; storage keys and the navigation event lost the app name.
- Not done: the root barrel is still the generated export-everything index (with the deleted modules removed, the three new ones added); `lib/logs.ts` and `lib/shell.ts` still bind to Tauri (frontend logging, clipboard); the production builders still send the wire value `genright_campaign_stack`, which `rightkit-media-pipeline` writes into saved packs (renaming it is a data migration). CutRight's `video-ui` keeps its own `PromptPanel` and `genright.ts`; it can now take `MediaGenClient` and the generate panels instead.

## Versions

| Package | Version | Kind |
|---|---|---|
| `rightkit-media-gen` | 0.2.0 | breaking |
| `rightkit-prompt-recipes` | 0.2.0 | breaking |
| `rightkit-remote-compute` | 0.2.0 | breaking |
| `rightkit-media-gen-models` | 0.1.0 | new (PK240) |
| `rightkit-media-pipeline` | 0.2.0 | breaking (requires media-gen 0.2.0, whose types it exposes) |
| `rightkit-media-gen-approvals` | 0.1.0 (unpublished) | requirement raised in the open entry |
| `rightkit-local-gen` | 0.1.2 | additive (`profiles::CATALOGUE_JSON`) |
| `@rightkit/media-gen-ui` | 0.2.0 | breaking |
| `@rightkit/media-ui` | 0.3.5 (unpublished) | additive (`invert`), in the open entry |

Recipes: PK240 (`rightkit-media-gen-models`: command table, route settings on a temp `Context`), and the recipes of `rightkit-media-gen` (typed request, neutral token domain), `rightkit-prompt-recipes` (pure library: contracts, no shipped brands, host brands and ventures), `rightkit-remote-compute` (worker commands only, worker store), `rightkit-local-gen` (`CATALOGUE_JSON`), `rightkit-media-gen-approvals` and `rightkit-media-pipeline` (pins), and npm PK67 `@rightkit/media-gen-ui` (closure without the shell packages; root exposes `setMediaGenClient`, `setHostBrands`, `MaskEditor`).
