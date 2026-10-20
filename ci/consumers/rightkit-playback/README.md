# rightkit-playback 0.3.0 registry consumer (draft, PK18)

Standalone consumer, outside the workspace, pinning `rightkit-playback =0.3.0` exactly with the
`tauri` feature, which is the feature CutRight uses. It exercises the authorisation path only:
`allowed_roots` with `set_authoriser` (CutRight `lib.rs:89-90`) and `authorised_media_source_id`
(CutRight `lib.rs:214`, through its `PlaybackState` method).

State: temp files in a temp directory, removed on drop. No ffmpeg is run, so no ffmpeg asset is
needed. No network, no user data.

Not covered: `prepare_registered_media_playback` and `compatibility_copy`, which run ffmpeg and
remain under the crate's asset-gated `remux_e2e` entry in `ci/asset-gated.json`. Also not covered:
the Tauri command wrappers, which need a live `tauri::State`, and `set_asset_scope`, which CutRight
does not call.

Note for the reviewer: CutRight pins `=0.2.2` (`apps/cutright/src-tauri/Cargo.toml:43`), so this
recipe proves the 0.3.0 surface that CutRight's source already uses, not its current pin.
