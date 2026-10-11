# RightSites cutover audit

Current source: `6612d4de5b09`. Reviewed 97 catalog rows: **2 Y**, **6 N**, **0 M**, **89 NA**.

Maintained targets: Node Hono API, React/Qwik web surfaces, Workers, `SellRightAdmin` iOS app & `SellRightWidgets` Live Activity extension. `ios/project.yml` wires both native targets; neither is copied/unreachable legacy source.

| Status | Component | Evidence | Concrete task |
|---|---|---|---|
| Y | `@rightkit/legal` | API imports `assertLegalManifest`; checkout calls it before receipt creation. | None |
| Y | `RightKitDiagnostics` | App target links product; live `Diagnostics.record` callers route via JSONL store + redactor. | None |
| N | `@rightkit/qa` | `scripts/qa-site-probes.mjs` shells to external CDP runner. | Replace runner with `@rightkit/qa` harness, preserve probes. |
| N | `@rightkit/qa-chrome` | Same probe runner uses installed Chrome outside RightKit. | Adopt managed Chrome only with browser policy. |
| N | `@rightkit/theme` | Admin hook owns persisted theme, root attributes, OS listener. | Map existing tokens/IDs without changing merchant palette policy. |
| N | `@rightkit/update` | Root invokes direct `pnpm outdated/up`. | Add manifest & gates before cutover. |
| N | `RightKitAppleCredentials` | `AppSession` actively calls local Security Keychain wrapper. | Link product, map token service/account/scope, then remove wrapper. |
| N | `RightKitLiveActivity` | App controller, push manager & widget extension actively own order lifecycle/presentation. | Link product, retain order contract/UI, migrate lifecycle guards. |

Cargo & Tauri rows remain NA: source census finds no Cargo/Tauri bridge. Other Swift rows are NA: maintained iOS targets link only `RightKitDiagnostics`; source has no corresponding imports or feature routes. `@rightkit/release` remains NA: current `r2-rightsites` API/server release surface has no RightKit desktop Windows/macOS consumer; no OCI/GHCR config appears in operative source. No builds, installs, network, product edits, or commits ran.
