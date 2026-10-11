# PostRight RightKit cutover matrix

Reviewed current PostRight source HEAD `e4720dd636a0352582ef8e7f25b4f2f9ef52a7b7` at `/Volumes/D/claude/postright`; targets: macOS & Windows Tauri desktop.

Counts: **Y 17 · N 1 · M 0 · NA 79** across all 97 catalog components.

PostRight is a scoped desktop application, not a maintained template: Tauri starts `postright-engine`; active product commands import/list channel proofs; renderer exposes engine doctor. Fixtures remain test-only.

Active shared routes cover Cargo crash, db, fs, http, jobs, logs, process, QA, secrets, service, settings, shell & tauri; npm app-shell, logs, platform-ui & release.

Cutover work:

- **Channel memory:** map fixture import into `rightkit-channel-memory` parse/write/citation rules before local persistence; retain channel-table policy.

Feature activation/delivery gap: updater plugin registration plus placeholder endpoint/key do not activate updates. `src/lib/updates.ts` is unreachable from active renderer path, so no check, download, or install caller exists. `npm:@rightkit/updates` is NA, not N; add live shared-client route & production endpoint/key before update delivery claim.

All Swift rows are NA: no SwiftPM/Xcode/iOS/watchOS target. Full evidence is in [postright.json](./postright.json).
