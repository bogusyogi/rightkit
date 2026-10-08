# RightKit ownership report

> **Regeneration failed (2026-10-08).** `node packages/release/cli/right-release.mjs ownership-report` ran for the 13 configured apps and exited 1. Nine apps (viewright, scraperight, heardright, mailright, coderight, cutright, genright, rightsites, voiceright) were marked `unscanned` with `RK_OWNERSHIP_POLICY_DOWNGRADE`: the catalog digest is below the protected pinned digest. Three repositories (pulse, postright, legion) were marked `unscanned` with `RK_OWNERSHIP_INVALID_REGISTRATION`: not registered in protected trust. membrane was marked `unscanned` because its configured app root is missing. No app was scanned, so the table shows no findings and no commits. The table is the generator's output, not hand-written numbers. The previous version of this file (generated 2026-10-07, catalog 1) is replaced and its figures are not current. Rerun the command after the protected trust accepts the catalog.

Generated: 2026-10-08T17:54:36.327Z · Catalog: 3.0.0 · Digest: 00687702864acbff234ce19991b0f4e17f9ca2666ec8b7f5204e1b82a5218529
Host coverage: none

| App | Scan scope | Status | Commit | Host | Scan age | Findings | Coverage gaps |
|---|---|---|---|---|---:|---:|---:|
| viewright | . | unscanned | unknown | unknown | 0s | 0 | 1 |
| scraperight | . | unscanned | unknown | unknown | 0s | 0 | 1 |
| heardright | tauri-app-next | unscanned | unknown | unknown | 0s | 0 | 1 |
| mailright | . | unscanned | unknown | unknown | 0s | 0 | 1 |
| coderight | repository | unscanned | unknown | unknown | 0s | 0 | 1 |
| cutright | . | unscanned | unknown | unknown | 0s | 0 | 1 |
| genright | . | unscanned | unknown | unknown | 0s | 0 | 1 |
| rightsites | . | unscanned | unknown | unknown | 0s | 0 | 1 |
| voiceright | . | unscanned | unknown | unknown | 0s | 0 | 1 |
| pulse | repository | unscanned | unknown | unknown | 0s | 0 | 1 |
| postright | repository | unscanned | unknown | unknown | 0s | 0 | 1 |
| legion | repository | unscanned | unknown | unknown | 0s | 0 | 1 |
| membrane | context/apps/membrane-hub | unscanned | unknown | unknown | 0s | 0 | 1 |

## Capability status

| App | Capability | Status | Detection | Availability |
|---|---|---|---|---|

Blocks: 2 · Exception requests awaiting verification: 0

Exception requests are unverified records; only scanner-validated signed exceptions are reported as excepted.
