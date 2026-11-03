# Publish handoff (2026-11-03 ~08:45 UTC) — read this first

Goal (Adrian): publish every RightKit crate and npm package; where publishing is blocked, port the apps onto what is published.
Owner now: the desktop agent. Approvals go to Adrian directly. Nothing below is in flight from the cloud session except the two runs named in §6.

This branch = `origin/main` (52d53551) + 2 unpushed-to-main commits + the master-plan docs + `publish-tools/`.
**Merge it into main only in a quiet window** (no `publish` / `publish-relay` run in progress): a push to main voids every in-flight publish dispatch (`main moved past SHA; requalify`).

## 1. State of the registries

Published this session: ffmpeg 0.2.13, automation 0.6.4, onnx 0.2.9, compose 0.2.2, privileged-helper 0.1.0, autoedit 0.1.1, document-workspace 0.1.2/0.1.3, embed 0.6.2, asr 0.4.0, dictation-core 0.2.1, browser 0.3.7, compositor 0.2.0, remote-gateway 0.1.0, social-inbox 0.1.1, motion 0.2.1, media-fetch 0.1.1, content-plan 0.1.1, agent-tools 0.3.1, credentials 0.4.4. npm: agent-ui-approvals, docx-editor, library-ui, workspace-ui 0.1.0, @rightkit/qa 0.6.2.

Qualified on main 52d53551, publish run still going at handoff (check `gh run list --repo bogusyogi/rightkit --workflow publish.yml`): memory-core 0.1.0, rightkit-qa 0.10.2, shorts 0.1.1, repo-graph 0.1.0.

Still unpublished (source version → what blocks it):

| Crate | Blocker / next step |
|---|---|
| agent-tools 0.3.2 | fix is on this branch (`HistoryStore::new(root, ".rightkit")`, wrap_untrusted_web_text_coderight); 0.3.1 is on crates.io with a non-compiling `coding_tools` feature. Needs push + qualify |
| agents 0.2.1, agent-orchestration 0.1.1 | failed only on agent-tools coding_tools compile; requalify after 0.3.2 is on main (their path dep resolves to the repo source) |
| tauri-shell 0.7.2 | needs credentials 0.4.4 (now published). Dispatched in a cancelled run; just requalify |
| dictation-text-pipeline 0.1.0 | fixtures vendored; `allowIgnored` set in ci/gate.config.json; needs credentials 0.4.4 (now published). Requalify |
| github 0.1.1, media-gen 0.1.0, remote-compute 0.1.0, library 0.1.1 | unblocked by credentials 0.4.4 / media-fetch 0.1.1; never qualified yet. Run clippy/rustdoc locally first (see §4) |
| web-session 0.1.0 | needs remote-gateway (published) — never qualified |
| approvals, media-pipeline | need media-gen |
| context-federation, context-runtime, context | need repo-graph, memory-core (qualified) |
| dictation-inference/command-host/runtime/engine | need text-pipeline |
| agent-engine-core, agent-engine-mobile-runtime, agent-demo | need agent-tools 0.3.2 (+ orchestration for mobile-runtime, tauri-shell for demo) |

npm unpublished: agent-ui-hooks, html-editor, markdown-editor, terminal-ui (qualified in CI; the four **publish** runs failed at Mac/Windows verify: "lock resolves @rightkit dependency outside reviewed closure: @rightkit/agent-ui-core | document-core"; fixed in `ci/consumer-recipes.json` on this branch — moved both from `declaredOnly` to `install` at 0.1.0; requalify), then agent-ui-components, agent-ui-settings, agent-ui-workbench, document-workspace (same closure check will bite: any @rightkit dep that is on the registry must be in `install`).
CI **does** publish npm (qa 0.6.2 went out through publish-direct); the Keychain `publish-local npm` route (rightkit#1493: prepack tsc exits 127 because the exported package has no node_modules) is NOT needed for these.

## 2. How a unit gets out (what actually works)

1. Push to main (preflight must pass: `node scripts/preflight.mjs` reads committed state only).
2. `gh workflow run qualify-direct.yml --ref main -f units="cargo:rightkit-x npm:@rightkit/y ..."` (MCP: workflow_id `qualify-direct.yml`, input `units`). Linux qualifies, then dispatches `publish.yml` (admission=direct) **only if main still == the run's SHA**. Mac + Windows verify run inside publish.yml; then publish-direct uploads.
3. No pushes to main from anyone between dispatch and the last publish run finishing (~20–30 min per batch). rkn froze pushes on request; Adrian now arbitrates.
4. Job logs: `gh api repos/bogusyogi/rightkit/actions/jobs/<id>/logs | grep -E 'stopped:|^[0-9T:.Z-]+ error'` — the real reason is the `rightkit-gate: unit … stopped:` line. (The MCP log tool truncates to the cleanup tail.) `RK_DIRECT_RELEASE_HELD` in a "failed" qualify job is the normal success message; the job turns red only if the dispatch step fails.

## 3. Bookkeeping when a published crate's source must change (bump)

Cargo.toml version; CHANGELOG entry; `packages/release/rightkit-versions.json` and its byte-identical copy `packages/build-control-policy/src/ownership-session-versions.json`; `ci/consumers/<crate>/Cargo.toml` pin; `ci/consumer-recipes.json` file hashes (cargo recipes: files only; `aggregateSha256` exists only on some — if present it is sha256 of compact JSON of the sorted `files` map); `Cargo.lock` of the crate and of every crate that path-depends on it; `node scripts/generate-index.mjs --workspace /home/user` (then `git checkout -- PORT-MATRIX* ADOPTION* port-matrix.*`). Unpublished versions are edited in place, no bump.
Exceptions (allow_ignored / allow_no_test / allow_missing_assets): `ci/gate.config.json` (`rust.<crate>.allowIgnored`) and `ci/rk-pipeline.json` `dispatch.exceptions` (exact version required) and `ci/asset-gated.json`.

## 4. Gate rules that cost rounds (check locally before dispatching)

- Class A is repo-wide and runs first: rustfmt (`cargo fmt` in each touched crate), notice digests, version pins, consumer recipes/locks.
- Per crate, each tested feature set gets clippy `-D warnings` for linux + macOS + Windows (zig cross, `node scripts/ci/provision-zig.mjs <dir>` → `RIGHTKIT_CROSS_CC_DIR`), then `cargo check` of `checkFeatureSets`, rustdoc `-D warnings`, then tests. `crossTargets` overrides are forbidden.
- Bundled SQLCipher cannot cross-build with zig (OpenSSL CommonCrypto / `os/log.h`) → credentials' `db-adapter` sets live in `checkFeatureSets` (native check only); `tests/db_adapter.rs` is not run by the gate.
- rustdoc: unresolved/private intra-doc links and bare `<tags>` fail; clippy: `int_plus_one`, `too_many_arguments`, `very_complex_type`, empty line after doc comment.
- Tests must not use `debug_assertions`, `test-mode`/`test-overrides` features or env switches; ignored tests need `allowIgnored`; zero tests needs `allow_no_test`; skipped tests need `allow_missing_assets`. Test binaries on Windows need a linkable SQLite (`rusqlite` with `bundled` as a dev-dependency when the tree contains libsqlite3-sys).
- rightkit-qa refuses Linux: its tests are `cfg(all(test, any(macos, windows)))` and the Linux lane has `allow_no_test`.
- Tauri units need the GTK/WebKit apt step (already in qualify-direct for `tauri` and `rightkit-qa`).
- `credentials` `memory-backend` feature: dev-dependency only; never in release.
- npm consumer recipes: `closure.install` must list every `@rightkit/*` dependency that exists on the registry; `declaredOnly` is only for unpublished ones. `ci/consumers/npm/<unit>/consumer.mjs` pins the expected version.

## 5. Local tooling (this branch: `docs/audits/2026-11-01-rightkit-package-master-plan/publish-tools/`)

- `ready2.py <list>` — which unpublished crates have all non-optional path deps on crates.io (run from repo root).
- `closure-sim.mjs` — recomputes npm recipe closures.
- `watch.sh` — registry poller (prints `PUBLISHED name version`).
- `unpub-v.txt` — the crate list used above.
Disk in the cloud box is ~2.5 GB; use `CARGO_TARGET_DIR=/tmp/x` per crate and delete after. A desktop host does not have that limit: run clippy + rustdoc (+ tests) for each remaining crate there before dispatching, one crate per round is what cost the time.

## 6. In flight / left running at handoff

- `publish` 41601917644 and `publish-relay` 41601936387 (credentials-batch crates): leave them.
- `qualify-direct` 41604238326 (tauri-shell, text-pipeline, github, media-gen, remote-compute, library): cancelled, no effect.
- Routine "Fleet PR sweep (hourly)" (trig_01SzCpWPvwRWfAca8iNXso7F) is not mine; it is still scheduled.
- `/tmp/rk-bht-*` and `/tmp/rk-admission-*` leftovers in the cloud box and `/home/user/rightkit-branch-cleanup.sh` (branch deletes the proxy blocked) need a human terminal.

## 7. Ordered next steps

1. Wait for publish 41601917644 / relay to end; confirm memory-core, rightkit-qa 0.10.2, shorts 0.1.1, repo-graph on crates.io.
2. Merge this branch's two code commits (agent-tools 0.3.2, npm closures) to main; preflight; push.
3. One qualify batch: agent-tools, agents, agent-orchestration, tauri-shell, dictation-text-pipeline, npm: agent-ui-hooks html-editor markdown-editor terminal-ui. Fix per §2 step 4.
4. Next batch as dependencies land: github, media-gen, remote-compute, library, web-session, repo-graph dependents (context-*), then dictation-inference/command-host/runtime/engine, approvals, media-pipeline, agent-engine-*, agent-demo. Pre-check each with clippy (3 targets) + rustdoc + tests first.
5. Remaining npm: agent-ui-components/settings/workbench, document-workspace (closure rule).
6. App ports: see `HANDOFF.md`, `PORT-EXECUTION.md`, `handoff/<app>.md`. App commits on `main` (heardright 3d183003..51cad537, genright db8446c..78cdafc, CutRight 9a54902..bf83904, pulse 0fe5beea/918878b9, mailright 7c28ce3) have **never been compiled**; their CI history is July. First job per app: install the published crates, build, fix.
7. RightKit-side gaps blocking ports: tasks #13/#14 and `handoff/` (dictation-core app_paths injection, hotkeys Serialize, overlay PillEvent, sync scheduler, fs known_folder, license gaps, mesh KNOWN_CAPS, bench extraction, ~24 crates still on `debug_assertions`, web-QA define spec rightkit#1480).
