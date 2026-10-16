# Breaking dependency updates, 2026-10-17

Compatible bumps were applied to the manifests (lockfiles refreshed separately). The entries below are semver-breaking and were NOT bumped. Latest versions are the newest stable, non-yanked releases from crates.io and the npm registry on 2026-10-17.

## Rust

| Dependency | Current | Latest | Used by | Migration note |
| --- | --- | --- | --- | --- |
| base64 | 0.22 | 0.23.1 | daemon, models, oauth, ocr, ownership, vault | Check the `Engine`/prelude API; mostly import changes |
| bindgen | 0.72 | 0.73.2 | tts-qwen-sys (build) | Regenerate bindings and diff the output |
| chacha20poly1305 | 0.10 | 0.11.0 | sync-keys | RustCrypto `aead` 0.6 line; move together with hkdf, sha2, x25519-dalek |
| ed25519-dalek | 2 | 3.0.0 | models | Moves to the newer `signature`/`rand_core` traits; align with sha2 0.11 |
| getrandom | 0.3 | 0.4.3 | credentials, device, mcp, models, oauth, qa, sync-keys, tts-qwen, workflow | `getrandom::fill` is the entry point; browser pins `=0.3.4` on purpose |
| hkdf | 0.12 | 0.13.0 | sync-keys | RustCrypto `digest` 0.11 line; move with sha2 0.11 |
| jni | 0.21.1 | 0.22.4 | android | Reworked `JNIEnv`/attach APIs; plan with the uniffi bump |
| lopdf | 0.44.0 | 0.45.0 | docs, pdf | Pre-1.0, read the changelog for object API changes |
| p256 | 0.13 | 0.14.0 | ownership | RustCrypto `elliptic-curve` line; move with spki and sha2 |
| quick-xml | 0.41.0 | 0.42.0 | docs | Pre-1.0, check reader/writer API changes |
| rcgen | 0.13 | 0.14.10 | daemon, http, send | Certificate params and key-pair API reshaped |
| reqwest | 0.12 | 0.13.5 | http | Feature flags and TLS defaults changed; recheck rustls setup |
| sha1 | 0.10 | 0.11.0 | ownership | RustCrypto `digest` 0.11 line; move with sha2 |
| sha2 | 0.10 / 0.10.9 | 0.11.1 | browser, job-queue, ocr, onnx, ownership, send, oauth, sync-keys | `digest` 0.11; `finalize()` output type is `hybrid-array`, so `.as_slice()` or `into()` where `GenericArray` was used. Repo already has sha2 0.11 in docs and pdf |
| spki | 0.7 | 0.8.1 | ownership | RustCrypto `der` line; move with p256 |
| tauri-winrt-notification | 0.7 | 0.8.1 | tauri-shell | Check the builder API |
| tokenizers | 0.22.2 | 0.23.2 | asr, embed | Check `Tokenizer` loading and padding/truncation API |
| toml | 0.8 | 0.9.8 | qa | `toml` 0.9 splits out `toml_edit`/serde parts; `from_str` and `to_string` unchanged for most uses |
| uniffi | 0.29 | 0.32.2 | android | Regenerate bindings; Kotlin side must match the same version |
| webview2-com | 0.38 | 0.39.1 | qa, qa-driver, tauri-shell | Moves with `windows` 0.62; bump together |
| windows | 0.61 | 0.62.2 | qa, qa-driver, tauri-shell | Repo already uses windows 0.62 elsewhere; align and move webview2-com together |
| windows-core | 0.62.2 | 0.100.0 | audio | Version jump to 0.100; confirm it still pairs with the `windows` crate in use |
| x25519-dalek | 2 | 3.0.0 | sync-keys | Moves with ed25519-dalek 3 and the `rand_core` bump |
| zip | 2 | 9.0.4 | crash, models, onnx | Large API gap; `ZipWriter` options type (`FileOptions`) changed |
| zip | 8.6.0 | 9.0.4 | docs | One major; docs already on the newer line than the others |

Grouped upgrades worth doing together: the RustCrypto set (sha1, sha2, hkdf, chacha20poly1305, p256, spki, ed25519-dalek, x25519-dalek), the Windows set (windows, webview2-com, tauri-winrt-notification), and zip across all four crates.

### Left as exact pins (not touched)

| Dependency | Pin | Used by | Reason |
| --- | --- | --- | --- |
| ort | =2.0.0-rc.13 | onnx | Suite pin, "every Right Suite crate and app uses exactly this ort version" (README Version policy). Latest stable is on a different line |
| async-tungstenite | =0.32.1 | browser | Exact pin, no reason recorded in the manifest |
| getrandom | =0.3.4 | browser | Exact pin (Windows target), no reason recorded |
| trybuild | =1.0.116 | child-process (dev) | Exact pin, no reason recorded |
| sysinfo | =0.37.2 | send | Exact pin, no reason recorded |

### Internal rightkit-* requirements older than the in-repo version (not changed)

Most are lower-bound path requirements, which stay valid. The largest gaps: rightkit-fs 0.4.0 vs 0.4.2 (18 uses), rightkit-child-process 0.4.0 vs 0.4.2 (18), rightkit-http 0.3.0 vs 0.3.2 (6), rightkit-daemon 0.2.0 vs 0.2.4 (6), rightkit-sidecar 0.2.0 vs 0.2.1 (5), rightkit-onnx 0.2.2 vs 0.2.6 (5), rightkit-ffmpeg 0.2.3 vs 0.2.7 (4), rightkit-automation 0.4.0 vs 0.4.3 (3). Smaller gaps exist for qa, credentials, browser, vad, tts-qwen-sys, speech-client, models, logs, license, job-queue, db and app-settings.

## npm

`peerDependencies` ranges were left alone (they are consumer compatibility contracts). `packages/sync-worker` is excluded in pnpm-workspace.yaml and was skipped. `@rightkit/*` references were left as they are.

| Dependency | Current | Latest | Used by | Migration note |
| --- | --- | --- | --- | --- |
| typescript | ^5.7.0 (now ^5.9.3) | 7.0.2 | all packages with a TS toolchain (19) | Major rewrite; check tsconfig options and `tsc` flags, and typescript-eslint support, before moving |
| vitest | ^2.1.8 (now ^2.1.9) | 5.0.5 | 18 packages | Three majors; needs a newer vite and Node, config and coverage options changed |
| vite | ^6.0.0 (now ^6.4.4) | 8.3.4 | onboarding, react-shell, settings-ui | Two majors; check plugin compatibility and Node version |
| jsdom | ^25.0.1 | 30.1.3 | i18n, legal-ui, media-ui, onboarding, settings-ui | Needs a newer Node; canvas/fabric usage in media-ui should be rechecked |
| @testing-library/jest-dom | ^6.6.3 (now ^6.9.1) | 7.0.1 | i18n, legal-ui, onboarding, settings-ui | Check the matcher setup import path |
| @types/node | ^22.10.0 (now ^22.20.5) | 26.6.5 | legal, license | Keep matched to the Node runtime in CI |
| @types/node | ^24.0.0 (now ^24.19.2) | 26.6.5 | outdated, qa-chrome, qa, theme | Keep matched to the Node runtime in CI |

Bumping vitest, vite and jsdom is best done as one change since they constrain each other.

Note: pnpm's `minimumReleaseAge` setting may refuse very recent releases when the lockfile is refreshed; check any resolution failure against that.
