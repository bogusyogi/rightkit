# Changelog

## Unreleased

Folds into 0.2.0 (unpublished). Found by diffing GenRight's `genright-gen` against this crate so a host can drop its copy; every default keeps RightKit's behaviour.

- Added `host::MediaHostConfig`, `host::configure_media` and `host::media_config` (set once at startup, beside `host::configure`): `app_name` (display name in user-visible messages; default none, which keeps the neutral wording "this app" / "the host" / "the catalogue"), `log_prefix` (stderr diagnostics; default `rightkit-media-gen`), `user_agent` (stock search and downloads; default `RightKit-media-gen/0.1 (asset acquisition)`) and `spend_attach_lock_name` (default `spend-attach.lock`; a host with existing locks passes its name). `host::app_or` picks the name or the neutral word.
- Changed: environment variables are read under the host prefix first, then `RIGHTKIT_`: the budget (`<PREFIX>_BUDGET_PROJECT`, `<PREFIX>_BUDGET_REMAINING`, also in the "a budget needs both ..." message), Blender (`<PREFIX>_BLENDER_PATH`, `_BLENDER_ASSETS_DIR`), Wan2GP (`_WAN2GP_DIR`, `_WAN2GP_PYTHON`), `_LIPSYNC_MODEL_DIR`. With the default prefix nothing changes. New `ledger::budget_project_env`, `budget_remaining_env`, `local_wan2gp::dir_env` and `python_env` return the host-prefixed names (the `*_ENV` constants stay and name the `RIGHTKIT_` ones). The worker still reads `RIGHTKIT_BLENDER_ASSETS_DIR` (`local_blender::ASSETS_ENV`), which the provider sets on the Blender process. The `FFMPEG` hint in the lip-sync and voice "no verified ffmpeg" messages and the scratch folder names (`<machine_dir_name>-lipsync-<pid>`, `-realesrgan-`) follow the host too.
- Removed (breaking): the test and dev environment reads GenRight deleted on 2026-11-03: `<PREFIX>_BLENDER_WORKER_DIR` (the worker is found only by discovery), `<PREFIX>_WAN2GP_JOBS` (`local_wan2gp::JOBS_ENV` is gone; `jobs_dir()` is `<organisation root>/wan2gp-jobs`) and `<PREFIX>_MUSICGEN_MODEL_DIR`.
- Changed: Blender renders have a 150-minute deadline across all attempts of a job, persisted in `deadline.json` beside the job (a restart or a retry keeps only the time left; an unreadable file is an error, never a fresh budget). Expiry ends the process tree and the job fails with the typed code `worker_deadline_exceeded` (was: a two-hour per-attempt limit and a plain failure message). Ported from GenRight, which shipped it first.
- Changed: WaveSpeed lipsync-3 is priced per second of input video ($0.135/s; the reservation is the 300 s cap, $40.50; it was a flat $0.05 per call). Ported from GenRight's 2026-11-03 pricing from the official page.
- Changed: fal video requests send only `16:9` or `9:16` as `aspect_ratio` (anything else becomes `auto` or `16:9`, as before the value was passed through unchecked). Ported from GenRight.
- Added catalogue entry `bytedance/seedance-2.5/talking-avatar` (WaveSpeed avatar, priced per audio second), from GenRight's catalogue.

## 0.2.0

- Removed (breaking): `media`, GenRight's container probe, a second copy of `rightkit_ffmpeg::quickprobe` (the extended one: size, codec, audio codec and frame count). Use `rightkit_ffmpeg::quickprobe` (`probe`, `probe_prefix`, `probe_file`, `probe_media`, `sniff_mime`, `PROBE_PREFIX_BYTES`); the descriptor, import and stock paths now call it.
- Removed (breaking): `host::h264`, a copy of `rightkit_ffmpeg::h264_rule` (19 differing lines, all path prefixes). Use `rightkit_ffmpeg::h264_rule`; lip-sync and `rightkit-media-pipeline` call it.
- Added `typed`: the typed request and result API beside the JSON commands. `MediaGen::new(ctx)` with `estimate`, `submit` (`GenerateRequest` to `Submitted`), `run`, `status` (`JobRecord`), `wait` (`Waited`), `result` (`JobResult` of `Asset`s), `cancel` and `ack`. It is the same path as `api` (validation, routing, spend reservation, durable job, library descriptor); fields the engine adds later are kept in each type's `extra` map.
- Added `store::setting_json` and `store::set_setting_json`: one JSON setting in the `settings` table. `avatars` uses them; `rightkit-media-gen-models` and `rightkit-remote-compute` use them instead of their own copies.
- Changed (breaking): the approval token domain is the host's. `host::approval::set_token_domain` installs it once; the default is `DEFAULT_TOKEN_DOMAIN` (`rightkit-approval-v1`). A host whose clients mint tokens under GenRight's `genright-approval-v1` sets that.
- Changed: the spend ledger's sidecar tables are `media_gen_spend_attempts` and `media_gen_spend_meta`. A database with the old `genright_spend_*` tables is renamed in place on attach, in one transaction, so attempts and schema version carry over.
- Renamed (breaking): `providers::orthic_root` is `providers::tools_root` (the shared tools root, unchanged); the temporary fallback folder takes the host's `machine_dir_name` instead of an organisation name.
- Not done here (listed in the design note): the spend layer thinned onto `rightkit-spend`'s multi-cap reserve, device allowance, invoice reconciliation and budgets; the provider adapters split from the engine core; voices to `rightkit-tts`; local providers to `rightkit-local-gen`.
- Taken on the recommended option of the 2026-11-11 shape review, pending the owner's confirmation (docs/audits/2026-11-11-shape-matrix/reports/genright-redraw.md).

## 0.1.2

- Added `gallery`: GenRight's gallery index (`index_roots`, `IndexFilter`, `GalleryResponse`, `GalleryReport`, `classify`, the brand catalogue and the eyes and jury state lists). It sat in `rightkit-library` as `src/artifacts.rs`, undeclared and never compiled; the shape review (2026-11-11, `rightkit-library` row) gave it its home here, next to the sidecars it reads. Its copies of this crate's names are gone: it uses `host::CALLER_SIDECAR_SUFFIX`, `AssetFacts::unknown()`, `library`'s `pair.json` name and the new `provenance::sibling_facts_path`.
- Added `host::approval::eyes_file_hash` and `host::approval::eyes_record_bound`: the eyes gate's hash and approval-record rule, which `rightkit-media-gen-approvals` and the gallery each had a copy of. `rightkit-media-gen-approvals` 0.1.0 now calls these.
- Added `provenance::sibling_facts_path`; `write_sibling_facts` uses it.
- Additive. Taken on the recommended option, pending the owner's confirmation.

## 0.1.1 (unpublished)

- Removed: the `dev-overrides` feature. ffmpeg and ffprobe resolve only through the catalog-verified bundled binary (`host::ffmpeg::trust` is always `verified`); the `<PREFIX>_FFMPEG`/`PATH` fallback and the `ORT_DYLIB_PATH` developer candidate in `qts::models` are gone. Adrian, 2026-11-03: "dev mode, is mine. im the fucking owner, my data. no dummy data, no fucking made up shit. real data. snapshot and restore." Needs rightkit-ffmpeg 0.2.16 and rightkit-onnx 0.2.10 (the resolver loses its `dev`/`mode` arguments).
- Changed (rightkit#1437): `host::keychain::store_for` guards with `GuardMode::Deny`; `GuardMode::Memory` no longer exists (rightkit-credentials 0.4.6). `<PREFIX>_NO_KEYCHAIN` now denies the credential store instead of swapping in an in-memory one. Adrian, 2026-11-03: "no dummy data, no fucking made up shit. real data. snapshot and restore."

## Unreleased

- The `capabilities` module documentation names `default_models` in plain code instead of linking it: the function is private and rustdoc refuses a public link to a private item under `-D warnings` (Mac release compile 2026-11-07).
- Removed `is_dev_storage_url`, left behind when the development-only provider base override was removed; it no longer compiles against `providers` without `PROVIDER_BASE_OVERRIDES` and could never match.

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from `genright-gen` (genright).
- `lease` (caller leases that tie a detached local-image runner to the process waiting for it) and `runner` (detached `<engine> job run <id>` runners, their scrubbed stderr logs and resume of recoverable jobs), moved from `genright-engine` `lease.rs` and `worker.rs`. The host engine binary must answer `job run <id>` and `job cancel <id>`.
- `media_commands`: the `media.*` command surface (`media.modalities`, `media.generate`, `media.route`, `media.route_prefs`, `media.still_finish`) and its CLI word map, moved from `genright-engine` `cli_media.rs`; still finishing runs on `rightkit-compose` (feature `still-finish`).
- `voices`: the voice library (`studio.voices.*`: list, presets, provider voices, ElevenLabs and local clones, design, batch, preview, export, legacy voice-folder import), moved from `genright-studio` `voices.rs` with `data/voices/builtin.json`. The local voice-model installer is reached through `voices::set_install_hooks` (the host registers `rightkit-remote-compute`'s model installer). The `studio.voices.import_voiceright` command name is kept for wire compatibility.
- `reference`: `reference.save_png` (store a UI-made PNG such as a painted mask under `<app data>/references/` so a generate request can name it), moved from `genright-engine` `reference_cmd.rs`.
