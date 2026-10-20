# Changelog

## 0.3.0 — unpublished staging (2026-10-18)

- New: `set_asset_scope` lets the host narrow the asset protocol's file scope (issue bogusyogi/rightkit#359, from CutRight). Tests: `tests/asset_scope.rs`.
- Added an opt-in host authorisation hook for media sources. `SourceRegistry::set_authoriser(Fn(&Path) -> bool + Send + Sync)`, `clear_authoriser`, and `resolve_or_register(path)` register any file the hook allows. `allowed_roots(roots)` builds a hook for "regular files under these directories, recursively".
- The path is canonicalised first (symlinks resolved, `..` removed) and the hook and registry see only the canonical form; a symlink inside an allowed root that points outside is refused, as are missing paths, directories and non-regular files. Roots and candidates are canonicalised the same way, so Windows verbatim prefixes and on-disk case compare consistently.
- Hook-registered ids are re-authorised every time they are resolved, so a hook that starts returning `false`, a cleared hook, or a path that now resolves elsewhere refuses the id with `PLAYBACK_NOT_ALLOWED`. Explicitly registered ids do not depend on the hook.
- New `PlaybackError::NotAllowed` (`PLAYBACK_NOT_ALLOWED`). The enum is not `#[non_exhaustive]`, so hosts that match it exhaustively must add the arm; this is a breaking change for 0.x semver, hence the minor bump.
- Feature `tauri`: `PlaybackState::set_authoriser`, `allow_roots`, `clear_authoriser`, `authorised_media_source_id`, and the command `authorised_media_source_id(path)`. The existing `media_source_id` command is unchanged and still never registers a path. Default behaviour without a hook is unchanged.

## 0.2.2 (2026-10-17)

- Depend on rightkit-child-process 0.5.0 (and rightkit-sandbox 0.5.0 where used); no behaviour change.

## 0.2.1

- Republished on the renamed dependencies: `rightkit-ffmpeg` (was `rightkit-media`), `rightkit-child-process` (was `rightkit-process`). The 0.2.0 on crates.io was packaged before the renames and still named the old crates.

## 0.2.0 - 2026-10-11

Additive release: registered media sources so the UI never supplies raw paths.

- Added `SourceId` (64-character lowercase hex, parsed with `SourceId::parse`) and `SourceRegistry`. `register(path)` is fingerprint-backed: the id derives from the canonical path, size and modification time, so a changed file fails closed (`PLAYBACK_MISSING`) until it is registered again, which issues a new id. `resolve(&id)` returns the canonical `&Path`. `revoke(&id)` and `revoke_ref` make a source unreachable; the file is never touched.
- `SourceRegistry::resolve_ref(reference)` accepts a `SourceId` string or a path that is already registered. Any other path is refused with `PLAYBACK_REQUEST`, even if the file exists.
- `PlaybackState` (feature `tauri`) now owns a source registry: `register_source(path)` and `revoke_source(reference)` are Rust-only entry points.
- Added Tauri commands `prepare_registered_media_playback(source, request_id)` and `revoke_media_source(source)`. The existing `prepare_media_playback(id, request_id)` and `cancel_media_playback` are unchanged.
- `PlaybackHost` is unchanged, so existing implementations keep compiling. The registry lives on `PlaybackState`, which owns the host.
- No change to derivative fingerprints or cache layout; cached derivatives stay valid.
- `PlaybackHost::registry()` (default `None`) lets the host share its `Arc<Mutex<SourceRegistry>>`;
  `PlaybackState::new` uses it, so the host's own commands and playback see one registry.
- `PlaybackState::with_registry(host, registry)` shares a registry explicitly and takes precedence over
  `PlaybackHost::registry()`.
- `PlaybackState::media_source_id(path)` and the Tauri command `media_source_id(path) -> String` return
  the id of a path that is already registered and unchanged. They never register a path: an unknown or
  changed path is refused with `PLAYBACK_REQUEST`.
- `revoke_media_source` and `PlaybackState::revoke_source` act on the shared registry.
  `prepare_media_playback(id, request_id)` is unchanged.

## 0.1.2 - 2026-10-10

- Drop `parking_lot`: the registry uses `std::sync::Mutex` and recovers a poisoned lock, which preserves the previous no-poisoning behaviour.
- Drop the pinned `sha2 =0.10.9`: the source fingerprint uses `rightkit_fs::digest::sha256_bytes` (`rightkit-fs` with only the `digest` feature). Fingerprints are byte-identical to before, so cached derivatives stay valid.
- No public API changes.
