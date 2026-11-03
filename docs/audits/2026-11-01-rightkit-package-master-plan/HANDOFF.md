# Port handoff (2026-11-03)

Read `PORT-EXECUTION.md`, `DECISIONS.md`, `per-app/<app>-rightkit-mapping.md` first. Rule: if the mapping says code moves, it moves; only SHELL/CONFIG/APPTEST/exceptions stay.

## State per app (pushed to the app's `main` unless noted; ALL UNVERIFIED: disk was <2G, no cargo/vite/tsc ran; locks hand-edited or `--lockfile-only`)
| App | Pushed to main | Lines removed | Left |
|---|---|---|---|
| heardright | 3d183003..51cad537 (7 commits) | ~8.1k | ~415k: dictation chain unpublished; see RightKit gaps |
| genright | db8446c, 7fb3635, 521d52b, 78cdafc | ~17.5k | ~100k: media-gen/studio crates (unpublished), UI 21.6k (`@rightkit/media-gen-ui` export map) |
| CutRight | 9a54902, 74a71a7, 8010a70, 8c0ec25, bf83904 | ~23.7k | ~40k Rust + playback/demo/Swift/scripts |
| pulse | 0fe5beea, 918878b9 (disk-cleanup, system-monitor) + branch `claude/port-wip` (agent stopped mid-run, 39 files, -30k) | ~? | mesh blocked (agent-files KNOWN_CAPS), app-manager blocked (home hook) |
| coderight | none on main; branch `claude/port-wip` (agent stopped mid-run, 184 files, -35k) | | largest app |
| mailright | branch `claude/port-wip` (3 files) | | |
| postright, scraperight, viewright, legion | agents stopped before any commit | 0 | all rows |

`claude/port-wip` branches are NOT verified and NOT merged. Continue from them: check compile with `cargo check --release -p <crate>` (never debug), then merge to main.

## RightKit-side blockers (tasks #13/#14 of the previous session)
- Publish: 40 crates + 9 npm (wave order in scratchpad `unpub-v.txt`; waves 0..6: automation/browser/ffmpeg/privileged-helper first). rightkit-asr 0.4.0 (Rust KWS, Sherpa removed) is on rightkit main, unpublished.
- Extend: dictation-core `app_paths` injection API (drop `test-mode`); hotkeys `HotkeyConflict: Serialize`; overlay pill model; sync event scheduler; fs public `known_folder`; license 7 gaps; local-gen `HostConfig` workspace name; rightkit-mesh `KNOWN_CAPS` + `agent-files`; app-manager home provider; spend (CallerBudget, SpendHook, reports); job-queue lease/worker; models governor; review eyes gate; tts voice library; engine-host progress sink / is_long; credentials `MemoryBackend` outside test/debug (breaks GenRight release build).
- npm: `@rightkit/dictation-ui`, `library-ui`, `media-gen-ui` need `./*` export maps (+ self-importing dist/styles.css bug in media-gen-ui).
- `rightkit-prompt-recipes`, `rightkit-blender-worker`, ml-lab, SQLCipher transcript store: MISSING-EXTRACT.
