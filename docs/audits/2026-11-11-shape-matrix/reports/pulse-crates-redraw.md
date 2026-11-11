# Pulse-referenced crates: redraw note

Date: 2026-11-11. Branch `claude/busy-hypatia-e30h7l`. Rows: `rightkit-adapt`, `rightkit-memory`, `rightkit-notch-ui`, `rightkit-launcher`, `rightkit-desktop-tweaks`, `rightkit-app-host` (`../matrix.md`), and the new `rightkit-agent-learning`, `rightkit-memory-client`, `rightkit-launcher-win`. Taken on the recommended option of the shape review, pending the owner's confirmation. Nothing here is on `main`.

The six crates were judged `wrong` because each carried Pulse's (or CodeRight's) product under a name that promised a shared capability, or two unrelated things under one name. The owner's goal is that every app moves most of its code into RightKit with no duplicate code; the reference order is HeardRight and ViewRight, then Pulse, then the rest. All six have Pulse as the reference app. All six are now `gaps`.

## A. `rightkit-adapt` (two subsystems, one name)

| Item | Before | After |
|---|---|---|
| `rightkit-adapt` | CodeRight's `coderight-learning` (0.1.x on the registry) | Pulse's governed behavioural learning (taste and insights), 0.2.0: the code of `rightkit-adapt-taste`, moved with `git mv`, unchanged; library `rightkit_adapt` |
| CodeRight's learning | `rightkit-adapt` | new `rightkit-agent-learning` 0.1.0 (moved with `git mv`), library `rightkit_agent_learning` |
| `rightkit-adapt-taste` | the Pulse code | 0.1.2 (unpublished) is `pub use rightkit_adapt::*`: every old path resolves, for one release; Pulse pins this name today |
| `harness_radar.rs` (532 lines, CodeRight's backlog watch list) | public module of the learning crate | private module `harness_radar_feed` of `rightkit-agent-server`, its only user (`/runtime/harness-radar`) |

Dependents: `rightkit-context-runtime` 0.1.1 uses `rightkit-adapt` (the types it exposes are the same items through the re-export); `rightkit-agent-engine-api-workflows` 0.2.0 (breaking: `CreateOptimizationRequest` carries the learning crate's types under its new name) and `rightkit-agent-server` 0.1.2 (unpublished) use `rightkit-agent-learning`.

Not done (gaps on `rightkit-agent-learning`): the two proposal stores. `LearningProposalStore` (in memory) and `ProposalStore` (SQLite) hold two different `Proposal` types, and `ProposalStore::upsert` repeats `rightkit_agent_engine_storage::WorkflowRepository::insert_learning_proposal`. Calling the repository was tried and reverted: it sits behind engine-storage's `workflow` feature, which pulls `rightkit-workflow` into the learning crate. Collapsing them means one `Proposal` type first.

## B. `rightkit-memory` (engine and client)

| Item | Before | After |
|---|---|---|
| `rightkit-memory` | CodeRight's client of Pulse's engine | the engine, 0.2.0: re-exports `rightkit-memory-core` at its root and `rightkit-memory-store` as `store`; no code of its own |
| CodeRight's client | `rightkit-memory` 0.1.x | new `rightkit-memory-client` 0.1.0 (moved with `git mv`), library `rightkit_memory_client` |
| `MemoryTier`, `MemoryEntry`, `default_scope` | declared twice (client `backend/types.rs`, core `types.rs`) | the core's, re-exported by the client; the core gains `MemoryTier::as_str`, `MemoryTier::parse` and `PartialEq` on `MemoryEntry` (`rightkit-memory-core` 0.1.1, additive). Same wire shape |
| `src/retriever.rs` (486 lines) | a second retrieval router, never compiled (not in `lib.rs`) | deleted |

Dependents: `rightkit-agent-engine-mobile-runtime` 0.1.1 and `rightkit-agent-server` 0.1.2 (both unpublished) use `rightkit-memory-client`. Both already linked ONNX and tiktoken through other crates, so the client's new dependency on the core costs them nothing.

Not done: `membrane.rs` to `rightkit-context-client`, host-supplied membrane binary locations (it still names Pulse.app and `notch-state.json`), the `agent_host` modules to `rightkit-agent-server`. Found while doing this: `rightkit-memory-core` (`absorbed.rs`) and `rightkit-memory-store` (`absorbed_records.rs`) declare the absorbed records twice, with fields in a different order; content hashes are computed over the serialised form, so making the store use the core's types needs a hash check first. Left as a gap on the `rightkit-memory` row.

## C. `rightkit-notch-ui` (Pulse's AI-usage cells)

0.2.0 (breaking): `layout::Cell` is a host value, `Cell::new(id, glyph, section)`, instead of Pulse's enum (`Cpu`, `Disk`, `Claude`, `Codex`, `Profile`, `Account`, `Send`, `Tools`). `Cell::ALL` and `CELL_COUNT` are gone; nothing is shown until the host calls `set_shown`, and `count()` is at least 1 so the geometry is always defined. The cell a minimised notch keeps (Pulse's Send cell) is `set_anchor(Some(cell))`; `send_cell()` is `anchor_cell()`. `card::Mark` is `None` or `Glyph(Glyph)`, and the Claude card's header layout is `CardContent::inline_head` for any card. Product words left the view model's documentation. Geometry, motion, hit-testing, the card model and the renderer are unchanged. Two `rustdoc` HTML errors in Windows-only docs (`<device>`, `<age>`) were fixed on the way.

`rightkit-clipboard-history` 0.2.0 follows (its Windows API shows `CardContent` and `Edge`). Not done: the hub colour settings still sit in process-wide atomics in `render.rs`; the Mac twin (`RightKitNotch`) does not exist.

## D. `rightkit-launcher` (Pulse's Windows launcher product)

| Item | Before | After |
|---|---|---|
| `rightkit-launcher` | everything, with Win32 behind `cfg(windows)` | 0.2.0: the portable core (`calc`, `commands`, `config`, `dictionary`, `files`, `notes`, `rates`, `json`); `host::Host` keeps translation, HTTPS, logging, file scope, ignore name, rates notice |
| Win32 panel | `panel`, `chat`, `snippets`, `spot_takeover`, `store` | new `rightkit-launcher-win` 0.1.0 (moved with `git mv`), with its own `host::PanelHost` |
| Pulse's pages, readings, cleanup file | seven hard-coded hub pages, Claude and Codex usage rows, `%LOCALAPPDATA%\Pulse\cleanup-findings-v1.json` | `PanelHost::app_pages`, `usage_rows`, `storage_pages`, `cleanup_summary` |
| Pulse section and strings | `Section::Pulse`, `launcherPulseCommands`, "Use Alt+Space for Pulse", "Pulse's assistant" | `Section::App` titled by `app_section_title` (configuration name `pulse` still read), switch `launcherAppCommands`, `product_name` in the Alt+Space question or no product at all; the stored answer `pulse` is still read, `app` is written |
| `json.rs` | identical file in `rightkit-launcher` and `rightkit-clipboard-history` | one copy in `rightkit-launcher`; clipboard-history re-exports it under its old path |
| `response_layout.rs` (AI answer model) | in the launcher | `rightkit_overlay_win::response_layout` (0.1.5, additive), as the plan already mapped Pulse's file; its four let-chains rewritten for edition 2021 |

Not done: the Mac launcher (Pulse's Swift `Launcher`, 12,919 lines) still has no home; FFI to this core or a Swift module is the owner's choice.

## E. `rightkit-desktop-tweaks` (a keyboard hook wired to Pulse)

The low-level hook itself was already `rightkit-hotkeys` (`LowLevelHooks`); no hook code is duplicated. What the crate added on top was a hub: one keyboard and one mouse hook shared by Pulse's features, with Pulse's wiring compiled in (screenshot chords and session keys, snippet forwarding, the tool wheel, the copy burst, the strip, the launcher type-ahead, one extra Alt-chord slot).

| Item | Before | After |
|---|---|---|
| hook hub | `rightkit-desktop-tweaks` `keys.rs` | `rightkit_hotkeys::windows::hub` (0.1.4, additive): `add_key_handler(order, ..)`, `add_mouse_handler`, `remove`, `KeyContext` (modifiers, Alt and Shift side, repeat), `physically_down`, `reinstall`, `own_tag`, `release_swallowed`; each hook runs only while it has handlers |
| shortcut layer | mixed into the hub | `rightkit-desktop-tweaks` 0.2.0: `keys::apply(Shortcuts { mac, save_find_tab_close, guards })`, one hub handler at `keys::ORDER`; `send_input` and `paste` tagged with the hub's tag; `services` keeps only logging |
| launcher type-ahead | `keys::capture_*`, reached through the launcher host | `rightkit_launcher_win::type_ahead` (a hub handler); `PanelHost::capture_*` removed |
| Pulse's screenshot, snippet, wheel, burst and strip wiring | `services::Services` callbacks inside the hook | the app's own hub handlers (before `keys::ORDER` when it must see an Alt chord first) |

Name: kept. What remains is a desktop tweak (Mac editing keys on Windows), and the review's own action was to keep the shortcut mapping here. Pulse's Mac conveniences, which the plan sent to this Windows crate, go to a planned Swift module `RightKitDesktopTweaks` (the Swift twin).

## F. `rightkit-app-host` (low-confidence verdict)

Read in full (129 lines): one trait, `AppHost`, every method defaulted (state folder, product name, profile-aware home and environment, published settings and state document), installed once per process and read by five crates moved out of Pulse: `rightkit-chat-bridge`, `rightkit-ai-accounts`, `rightkit-owner-approvals`, `rightkit-remote-gateway`, `rightkit-web-session`. It duplicates nothing: `rightkit-tauri-shell`, `rightkit-instance` and `rightkit-app-settings` keep no process-wide host; `rightkit_agent_config::state_root` computes an agent app's `Orthic Labs/<name>` root from `rightkit-roots` (an app that has one returns it from `AppHost::state_root`); the profile-aware home is `rightkit-fs` `profile_home`, which it calls. It is not in `packages/release/ownership-mandatory-set.json`. Recommendation: keep it, optional, for apps that embed those crates. README and an unpublished CHANGELOG line only.

## Plan mapping

`../../2026-11-01-rightkit-package-master-plan.md` (coverage note, rows, extend and adopt tables), `per-app/pulse-rightkit-mapping.md`, `per-app/coderight-rightkit-mapping.md`, `rules/pulse.tsv`, `rules/coderight.tsv`, `rules/_shellpass.tsv` and `packages.json` agree. Actions are kept, so the coverage totals per app do not change. Pulse line counts are `wc -l` of the files on Pulse's main (`keys.rs` 870 and `window_keys.rs` 928 reproduce the old 1,798 exactly).

| App | Code | Was | Now |
|---|---|---|---|
| CodeRight | learning, 8,073 | `rightkit-adapt` | `rightkit-agent-learning` |
| CodeRight | memory backend and membrane client, 9,759 | `rightkit-memory` | `rightkit-memory-client` |
| Pulse | Cortex, 14,843 | `rightkit-memory` | unchanged (the name is the engine now) |
| Pulse | membrane-adapt, 15,823 | `rightkit-adapt` | unchanged (now the right crate) |
| Pulse | `launcher.rs`, `launcher_snippets.rs`, `launcher_store.rs`, `spot_takeover.rs`, 7,234 | `rightkit-launcher` | `rightkit-launcher-win` |
| Pulse | the launcher core and the Mac launcher, 15,003 | `rightkit-launcher` | unchanged |
| Pulse | `render.rs`, `layout.rs`, `motion.rs`, `card.rs`, 7,190 | `rightkit-overlay-win` | `rightkit-notch-ui` (where the code already is) |
| Pulse | `keys.rs`, 870 | `rightkit-hotkeys` | `rightkit-desktop-tweaks` (the hub part is in `rightkit-hotkeys`) |
| Pulse | Mac conveniences and Finder extension, 2,168 | `rightkit-desktop-tweaks` | planned Swift `RightKitDesktopTweaks` |

The per-file inventory JSON in each app repo was not changed; the matrix's planned-lines cells still come from it.

## Versions

| Crate | Version | Kind |
|---|---|---|
| `rightkit-adapt` | 0.2.0 | breaking (new content under the name) |
| `rightkit-agent-learning` | 0.1.0 | new (renamed) |
| `rightkit-adapt-taste` | 0.1.2 (unpublished) | re-export of `rightkit-adapt` |
| `rightkit-memory` | 0.2.0 | breaking (engine facade) |
| `rightkit-memory-client` | 0.1.0 | new (renamed) |
| `rightkit-memory-core` | 0.1.1 | additive |
| `rightkit-notch-ui` | 0.2.0 | breaking |
| `rightkit-launcher` | 0.2.0 | breaking |
| `rightkit-launcher-win` | 0.1.0 | new |
| `rightkit-desktop-tweaks` | 0.2.0 | breaking |
| `rightkit-hotkeys` | 0.1.4 | additive (`windows::hub`) |
| `rightkit-overlay-win` | 0.1.5 | additive (`response_layout`) |
| `rightkit-clipboard-history` | 0.2.0 | breaking (requires notch-ui 0.2.0, whose types it shows) |
| `rightkit-agent-engine-api-workflows` | 0.2.0 | breaking (learning types under the new crate name) |
| `rightkit-context-runtime` | 0.1.1 | dependency rename |
| `rightkit-agent-server`, `rightkit-agent-engine-mobile-runtime` | 0.1.2, 0.1.1 (unpublished) | dependency renames in the open entries |
| `rightkit-app-host` | 0.1.1 (unpublished) | README |

Recipes: PK62 (`rightkit-adapt`, now the governed learning), PK61 (`rightkit-adapt-taste`, old paths through the re-export), PK110 (`rightkit-memory`, the engine through one crate), PK108 (`rightkit-memory-core`), PK186 (`rightkit-notch-ui`, host cells and anchor), PK184 (`rightkit-launcher`, core and JSON), PK179 (`rightkit-desktop-tweaks`), PK35 (`rightkit-hotkeys`, hub surface on Windows), PK175 (`rightkit-clipboard-history`), PK08 (`rightkit-overlay-win`, answer excerpt), the context-runtime and api-workflows recipes (pins), and new PK219 (`rightkit-agent-learning`), PK220 (`rightkit-memory-client`, the engine's tier at the client), PK221 (`rightkit-launcher-win`). Version feed and its copy, locks, the e2e notice digest and INDEX follow.

## Checks run

`cargo +1.99.1` clippy `--all-features -D warnings` on every changed crate (host), and for the Windows-only code also with `--target x86_64-pc-windows-msvc` (`rightkit-notch-ui`, `rightkit-launcher`, `rightkit-launcher-win`, `rightkit-hotkeys`, `rightkit-desktop-tweaks`, `rightkit-overlay-win`; `rightkit-clipboard-history` without `--all-features`, see below); `cargo doc --no-deps --all-features` with `-D warnings` on host and Windows targets; rustfmt `--check`; `cargo check` of `rightkit-context` and the other direct dependents; `cargo metadata --locked`; every changed recipe against path dependencies in a scratch copy (run on Linux; the Windows-only recipes skip there and were checked for the Windows target); `node scripts/preflight.mjs --worktree`. Nothing was run on Windows itself: the hub, the shortcut layer, the panel and the type-ahead are compiled, not exercised.

Known and unchanged: `rightkit-clipboard-history` with `--all-features` (`qa-views`) on Windows warns about two unused items in `strip.rs` and `strip/popover.rs`, which this redraw did not touch.
