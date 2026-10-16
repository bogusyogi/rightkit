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
| ort | =2.0.0-rc.14 (moved from rc.13, ember#230; Environment-handle migration, runtime stays 1.24.4 on `api-24`) | onnx | Suite pin, "every Right Suite crate and app uses exactly this ort version" (README Version policy). Latest stable is on a different line |
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

## Deferred

### quick-xml 0.41 to 0.42 (rightkit-docs)

Not bumped. 0.42 is a redesign of the byte API (changelog #963): names, events and attribute values move from `&[u8]`/`Vec<u8>` to `&str`/`String`/`Cow<str>`, `decode()` and `decoder()` are removed, `Reader` rejects non-UTF-8 input unless wrapped in `DecodingReader`, `BytesText` content methods return `Cow<str>` directly, `NamespaceError::TooManyDeclarations` becomes `TooManyBindings`. About 190 call sites in `crates/rightkit-docs/src/docx.rs`, `xlsx.rs` and `xmlutil.rs` touch the changed types, and the crate is a hostile-input parser, so a blind edit is not safe. Plan: (1) bump with a compiler in the loop, starting from `xmlutil.rs` (the shared reader helpers, including the `normalized_value` call) so docx and xlsx only see `&str`; (2) drop the manual UTF-8 and `from_utf8` conversions that become redundant, but keep the preflight DOCTYPE/entity refusal and its tests unchanged; (3) check the lower namespace-binding cap (256 to 128) against the preflight limits and the existing hostile-input fixtures; (4) re-run the docx and xlsx golden tests and the malformed-XML tests; (5) check the `` escaping change (#990, `` is now written as `&#13;`) for round-trip tests of rewritten parts.

### jni 0.21.1 to 0.22.4 (rightkit-android)

Not bumped. `jni` 0.22 reworks `JNIEnv`, the attach API and the native-method entry points, and about 70 call sites in `rightkit-android` (`context.rs`, `keystore.rs`, `jni_entry.rs`, `lib.rs`) use the 0.21 types (`JNIEnv<'_>`, `JavaVM::attach_current_thread`, `JValue`, `JClass::from(JObject::from_raw(..))`, `call_static_method`, `exception_*`). It also cannot move alone: `lib.rs::init_platform_verifier` hands `jni::JNIEnv` and `jni::objects::JObject` to `rustls_platform_verifier::android::init_with_env`, so the `jni` major must match the one `rustls-platform-verifier` 0.7 depends on (0.21). `rightkit-credentials` reaches JNI only through the `KeystoreBridge` trait that `rightkit-android` installs, so its code does not change. Plan: (1) wait for a `rustls-platform-verifier` release built on jni 0.22 (or drop the `platform-verifier` feature's JNI hand-off by initialising the verifier from Kotlin); (2) rewrite `ContextHandle::with_env_dyn` on the new attach closure API and keep `with_env` as the public seam; (3) port `keystore.rs` (`new_string`, `byte_array_from_slice`, `call_static_method`, exception reading) and `jni_entry.rs` (`nativeInit`) to the 0.22 equivalents; (4) test on a device, because nothing in this crate has run yet (README "UNVERIFIED").

### Notes on bumps done in this pass (verify on first build)

- reqwest 0.13 (rightkit-http): feature `rustls-tls` is now `rustls-no-provider`, which keeps the ring provider that `build_tls` installs through `use_preconfigured_tls` (plain `rustls` would pull in aws-lc). `use_preconfigured_tls` still exists in 0.13 (soft-deprecated alias of `tls_backend_preconfigured`); rename it when convenient.
- rcgen 0.14: `CertifiedKey` field `key_pair` is now `signing_key`; `signed_by` takes an `Issuer` (the test CA in `rightkit-http/tests/tls_local.rs` now holds a `CertifiedIssuer`).
- uniffi 0.32 (rightkit-android): Rust surface unchanged. Regenerate the Kotlin with `scripts/build-android.*`; the JNA pin (`5.14.0@aar`) in `android/build.gradle.kts` was not checked against the 0.30+ direct-mapping requirement.
- bindgen 0.73 (rightkit-tts-qwen-sys): builder calls unchanged; regenerated bindings may differ in layout and alignment output, so diff `bindings-<target>.rs` when next regenerated. The checked-in bindings were not touched.
- tokenizers 0.23, lopdf 0.45, toml 0.9, zip 9: no call-site changes needed (APIs used are unchanged).

### npm toolchain bump (manifests and configs edited, nothing installed or run yet)

Moved to typescript ^7.0.2, vitest ^5.0.5, vite ^8.3.4, jsdom ^30.1.3, @testing-library/jest-dom ^7.0.1 and @types/node ^26.6.5. CI and publish run Node 26.8.1, which meets vitest 5 (`^22.12 || ^24 || >=26`), jsdom 30 (`^22.22.2 || ^24.15 || >=26`) and jest-dom 7 (22+). pnpm-lock.yaml is refreshed separately.

No bail-out for TypeScript. TypeScript 7.0 ships no programmatic API (7.1 is expected to add one; `@typescript/typescript6` re-exports the 6.0 API for tools that need it). No script or package in this repo imports `typescript` (no `transpileModule`, `createProgram` or `ts.*` use), vite 8 and vitest 5 do not import it, and typescript-eslint is not used, so every package moves to 7.

Changes made, each checked against the official notes (TypeScript 7.0 and 6.0 release posts, the vitest v5 and v4 migration guides, the vite v8 migration guide, the jest-dom and jsdom GitHub releases):

- tsconfig (TS 6/7 default changes): `rootDir` now defaults to the tsconfig directory, so `"rootDir": "src"` was added to doc-render, i18n, legal-ui, media-ui, onboarding, pdf-ui, platform-ui, react-shell, settings-ui and theme to keep the `dist/index.js` layout. `types` now defaults to `[]`, so `"types": ["node"]` was added to qa and qa-chrome (their non-test sources use node globals; legal, outdated and theme already had it). No tsconfig uses a removed option (`baseUrl`, `moduleResolution node`, `target es5`, `downlevelIteration`, `esModuleInterop: false`). `noUncheckedSideEffectImports` is now on: the only side-effect import in `src` is the css one in media-ui, covered by `src/css.d.ts` (`declare module "*.css"`).
- `tsc` CLI: file paths on the command line are an error when a tsconfig.json is in the cwd unless `--ignoreConfig` is passed, so `packages/tauri` `typecheck:contracts` now passes `--ignoreConfig`.
- vite 8 (Rolldown and Oxc): `esbuild: { jsx: "automatic" }` is deprecated and was replaced by `oxc: { jsx: { runtime: "automatic" } }` in the react-shell and settings-ui e2e harness configs (also added to the onboarding harness for parity); `build.rollupOptions` was renamed to `build.rolldownOptions` in the react-shell harness. Vite 8 applies a tsconfig only to files its `include`/`exclude` match, and every package tsconfig excludes the test files, so the jsx runtime is now set explicitly in the vitest configs of i18n, legal-ui, onboarding, react-shell and settings-ui. New minimal `vitest.config.ts` files (jsx runtime only) were added to media-ui, platform-ui and pdf-ui, which run tsx tests with no config.
- vitest 5: `dist` is no longer excluded by default (changed in vitest 4), so `**/dist/**` was added to the qa `exclude`; outdated already had it. The other configs only use `environment`, `include`, `setupFiles`, `testTimeout`, `hookTimeout`, `fileParallelism` and `resolve.dedupe`, which are unchanged. `vite` is now a peer dependency of vitest (pnpm auto-installs it). A grep of the tests found no `.sequential`, nested `vi.mock`, `toThrow("")`, constructor mocks or `poolOptions`.
- jest-dom 7: `@testing-library/dom` is now a required peer, so `"@testing-library/dom": "^10.4.2"` (the version media-ui already uses) was added to i18n, legal-ui, onboarding and settings-ui. The `@testing-library/jest-dom/vitest` import in the setup and test files is unchanged.

Verify on first run: `clearMocks` now defaults to true in vitest 5 (mock call history is cleared before every test; set `clearMocks: false` in a config if a test relied on cross-test history), jsdom 26 to 30 changed CSS and computed-style behaviour, and the `// @vitest-environment jsdom` docblocks in media-ui and settings-ui should still be honoured.

Deferred:

- packages/sync-worker (excluded from the workspace, parked 2026-10-16): left on typescript ^5.7.0 and vitest ~2.1.9, because `@cloudflare/vitest-pool-workers` ^0.8.0 targets the vitest 2.x line and `poolOptions` was removed in vitest 4.
- desktop-app-template (own pnpm-lock.yaml and workspace file): already on typescript ^6.0.3 and vite ^8.2.2, left unchanged. Moving it to 7 only changes `typescript`; its tsconfig is `noEmit`, and `src/vite-env.d.ts` already references `vite/client` explicitly.
- packages/release/package.json: nothing to change (it lists none of the bumped dependencies).
