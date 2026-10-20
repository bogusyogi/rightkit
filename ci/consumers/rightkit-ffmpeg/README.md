# rightkit-ffmpeg 0.2.10 (PK19 motion dependency, draft consumer recipe)

Standalone consumer, outside the workspace, pinning `rightkit-ffmpeg =0.2.10` exactly. It uses only
the pure, asset-free API: the argument builders `pcm_s16le_16khz_mono_args`,
`command::extract_args` and `command::transcode_audio_command`; the `-encoders` listing parser
`EncoderInventory::parse` with `pick_h264` on a fixed listing; and the embedded artifact catalog
(`ArtifactCatalog::embedded`, plus `parse` refusing an unsupported schema).

State: none. FFmpeg is never resolved, verified or run; no network, no files, no real user data.
Run only by the CI consumer lane.

Not covered: `verify_ffmpeg`, `verify_ffprobe`, `run_ffmpeg`, the decode and encode paths, digest
checks of any binary, and the `dev-overrides` feature.
