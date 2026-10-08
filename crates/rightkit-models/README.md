# rightkit-models

Shared local-model manifest and installer for Right Suite apps: model revisions
and quantizations, resumable verified downloads (ETag/Range, SHA-256), hardened
archive extraction (tar.gz/zip), atomic activation with rollback, and hardware
fit (disk, RAM, GPU) preflight. Product-neutral; apps supply the manifest.

Also (0.2.0):

- `Store::adopt` / `adopt_with`: verify an existing install against the pins
  and record it without downloading (hard link on the same volume, else copy;
  atomic swap; fails closed, never records a partial adoption).
- `Store::resolve` / `resolve_or_legacy`, and the opt-in legacy flat alias
  (`Store::with_legacy_alias_root`: `<alias root>/<id>` links to the active
  revision, maintained on install, adopt, rollback, removal, uninstall and GC).
- Disk preflight credits staged, same-key installed (hard-linked) and shared
  files whose pins match (`Event::PreflightCredit`).
- Shared runtimes: `FileSpec::shared = Some(kind)` places a file under the
  app-configured `Store::with_shared_root(kind, dir)`; traversal-checked,
  pinned, never overwritten (`ErrorCode::Conflict`), kept on uninstall.
- `InstallOptions::pause` keeps staging and resumes (HTTP Range with a strong
  ETag, else a clean restart); `cancel` discards staging. `Store::paused`,
  `Store::discard_staging`.
- `Requirements::{cuda_vram_gb, mps_memory_gb, remote_only}`,
  `FitKind::{RentalOnly, Api}`, `Hardware::probe_with(HardwareOverride)`
  (app-supplied path or JSON, lenient shape), `ProfileAlias`.

Also (0.2.1):

- `Store::adopt_then_install`: partial adoption in one locked install: adopt
  every present, pin-matching file (hard link, else copy), download only the
  missing or mismatching ones, one atomic swap. Pins are required; nothing is
  recorded until every file verifies; preflight counts only bytes still needed.
- `Store::with_legacy_alias_map` / `with_legacy_alias_paths`: per-id alias
  locations, including inside the store root (`<root>/<id>/rN`, never a
  store-owned name) or a flat runtime dir allowed with
  `Store::with_legacy_alias_allowed_root`. Real paths are never replaced.
- `Store::with_url_rewrite(base)`: test/dev seam that sends downloads to a
  loopback mock (`http://` on `127.0.0.0/8`, `localhost` or `[::1]` only;
  never read from the environment). `Store::download_url` shows the result.

Also (0.2.2):

- Shared-runtime reference counting: `Store::register_shared(app, kind, version)` records that an app uses `<tools root>/<kind>/<version>` in `tools/refs` (the tools root is `tools` beside the store root, or `Store::with_tools_root`); `Store::unregister_shared(app)` drops them, and `Store::unregister_app` now does so too (its return value is still model ids).
- `Store::gc_shared(kinds)` removes, under the registry lock, unreferenced `tools/<kind>/<version>` directories for the listed kinds only (unlisted tools such as yt-dlp are never touched; an empty list removes nothing) and returns the `(kind, version)` pairs; symlinks are never followed, `tools/refs` is untouched, and a corrupt ref aborts with nothing removed.
- `suite::paths::shared_runtime_dir(kind, version)` / `shared_runtime_dir_at(tools_root, kind, version)` validate both as single safe path segments (`InvalidSpec` otherwise).

See `tests/install.rs`, `tests/adoption.rs`, `tests/partial_alias_rewrite.rs`
and `tests/e2e_resume.rs` (rightkit-qa `MockServer`: Range, dropped
connections, servers without resume) for end-to-end usage.

## RightApps promoted private artifacts (`rightapps` feature)

`RuntimeArtifactClient` resolves an artifact ID, current promotion row ID or
signed manifest pointer through `POST /v1/apps/:appKey/runtime-artifacts/resolve`.
Pass RightApps
`signedToken` from license activation/refresh, not
`activationToken` or a purchase key. Server checks current Pro entitlement,
active device & license status before minting an object-scoped download grant
that expires within five minutes. Model access does not require an active
software-update window.

Pin the raw 32-byte Ed25519 public key corresponding to
`rightkit-runtime-artifacts-2026-07` in application source/build configuration.
It must equal server `RIGHTSITES_RUNTIME_ARTIFACT_PUBLIC_KEY`; never trust a key
returned by resolve. Signing-key material is not needed in consumers.

```rust,no_run
# #[cfg(feature = "rightapps")]
# fn install(signed_token: &str, runtime_public_key: [u8; 32]) -> Result<(), Box<dyn std::error::Error>> {
use rightkit_models::{RuntimeArtifactClient, Store, InstallOptions};
use rightkit_models::rightapps::RuntimeArtifactTarget;

let client = RuntimeArtifactClient::new("https://api.spoares.com", runtime_public_key)?;
let artifact = client.resolve(
    "genright",
    "qwen3-customvoice",
    signed_token,
    &RuntimeArtifactTarget::current(),
)?;
let store = Store::new("/application-data/models", "genright"); // use app's native data directory
let installed = client.install(
    &artifact, &store, "qwen3-customvoice-talker", &InstallOptions::default(),
    |event| { /* forward progress, pause, activation events to UI */ },
)?;
let model_path = installed.dir.join(&artifact.envelope().manifest.object.filename);
# let _ = model_path;
# Ok(()) }
```

Run this blocking API on a worker thread (`spawn_blocking` in Tauri/Tokio).
Installation reuses model-store locks, signed SHA-256 & size pins, resume with
strong ETags, immutable digest revision directories & atomic activation. Raw
artifact bytes are installed; archives remain archives. Configure
`InstallOptions::pause` for retained partial downloads; cancellation discards
staging. After `ExpiredToken`, refresh the signed license token, resolve again
& install with the same model ID. An unchanged object digest resumes partials
even when its grant URL changes. Network errors retain staging too.

Errors include `NotEntitled`, `ExpiredToken`, `SignatureMismatch`,
`DigestMismatch`, `SizeMismatch`, `Network`, `InvalidManifest`, `NotFound` &
`Store`. Download URLs are sensitive capabilities: do not log or persist them;
`ResolvedRuntimeArtifact` Debug output omits its URL. Resolve & download do not
follow redirects or forward a license token to the download host.

OS-independent bytes use promotion target `{ "os": "any", "arch": "any" }`.
Consumers send their concrete native target. Half-wildcards are invalid. One
`artifactId` promotion serves its exact target or `any/any`; object keys always
bind digest & filename. Artifact IDs use lowercase slug syntax
`^[a-z0-9]+(?:-[a-z0-9]+)*$` with a 128-byte limit. A pointer explicitly names
its promotion slot; resolution never substitutes a different pointer. Legacy
manifests omit `artifactId` & retain their original signed JSON.

Enable `features = ["rightapps"]` on the exact registry version containing this
change after publication. This source change does not alter registry pins.

JS interoperability fixture regeneration (test-only deterministic key):

```sh
node crates/rightkit-models/tests/fixtures/generate-runtime-vectors.mjs
rightkit cargo test -p rightkit-models --features rightapps
```
