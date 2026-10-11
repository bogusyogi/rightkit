# Dependency removal skeptic — 2026-10-11

Fresh source-only review of removal candidates from `evidence/npm-review.md` & `rust-review.md`. No prior audit used; no build, test, install, network, ingest, or product edit ran.

## Accepted candidates

| Candidate | Current evidence | Decision | Required validation |
|---|---|---|---|
| `@rightkit/settings-ui` → `@rightkit/shell` peer + dev | Manifest declares peer/dev at `packages/settings-ui/package.json:33,42`; source says structural adapter needs no runtime or type import at `packages/settings-ui/src/shell-adapter.ts:3-5`; checked-in declaration also has no module specifier at `packages/settings-ui/dist/shell-adapter.d.ts:3-21`. | Remove peer, optional-peer metadata, & dev edge. | Focused typecheck/test & packed-consumer check. |
| `desktop-app-template/src-tauri` → `rightkit-service` | Shell imports `rightapp_protocol::app_data_root` at `desktop-app-template/src-tauri/src/lib.rs:12`; diagnostics does same at `src/diagnostics.rs:8`; protocol owns service edge at `crates/protocol/Cargo.toml:11` & resolves `Suite` at `crates/protocol/src/lib.rs:7,27-32`. | Remove direct template edge at `src-tauri/Cargo.toml:41`; retain protocol edge. | Compile template on supported targets. |

## Rejected or deferred candidates

| Candidate | Skeptic evidence | Decision |
|---|---|---|
| `rightkit-audio` → `windows-core` | Source invokes `#[implement(IMMNotificationClient)]` at `crates/rightkit-audio/src/lifecycle.rs:198`. Locked `windows-core 0.62.2` depends on `windows-implement 0.60.2` (`Cargo.lock:8338-8347`, `:8373-8381`); current cached macro generator emits `::windows_core::IUnknownImpl` at `/Users/adrdsouza/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/windows-implement-0.60.2/src/gen.rs:182-184`. Direct `windows-core` supplies macro-expanded absolute crate path despite source importing `windows::core`. | Reject removal; token absence is insufficient for proc-macro output. |
| `@rightkit/app-shell` → `@tauri-apps/plugin-os` peer | App-shell peer is optional at `packages/app-shell/package.json:41,75-78`; platform-ui owns dynamic loader plus optional peer at `packages/platform-ui/package.json:31-36,58-61` & `src/index.ts:47-82`. Npm peer resolution across a consumer's direct/peer graph needs packed-consumer proof. | Defer; no manifest edit from source-only evidence. |
| `@rightkit/update` → `@rightkit/git` production edge | Runtime source imports `@rightkit/git/drift.mjs` at `packages/update/src/check.ts:9`. `workspace:^` concerns publish rewrite or standalone packing, not dependency necessity. | Retain; validate pack rewrite separately. |
| `@rightkit/media-ui` image peers | Image entry statically imports optional image packages, while root remains fabric-free, per `evidence/npm-review.md:18`. | Retain; subpath contract needs packed-image smoke. |
| Type/build/test edges called out by review | Emitted declarations retain `mdast` in doc-render, `@rightkit/updates` in tauri, plus logs/updates in settings-ui (`evidence/npm-review.md:57`). Compiler, test, & build dependencies were intentionally classified in review scope. | No deletion claim. |

## Lens result

`evidence/lenses/reasoning.minimize.json` records two accepted low-severity findings with `verifyStatus: unverified`. Packet excerpt cache examined 29 of 1,996 denominator paths & omitted 1,967, so result declares `complete: false`.
