# rightkit-media-gen

Generative media providers (fal, WaveSpeed, Veo/Gemini, OpenAI Images, ElevenLabs, HeyGen, EvoLink, Higgsfield, Muapi, NanoBanana, Replicate, Wan2GP, Blender, local), model catalogue and facts, routing, durable generation jobs, result descriptors, library and spend ledger

Moved into RightKit from `genright-gen` (genright) on 2026-11-01.

## Host configuration

The crate carries no product names. A host configures it once through
`rightkit_media_gen::host::configure` (re-exported from `rightkit-local-gen`): the app identifier
(per-user data folder and OS credential-store service), the settings namespace (also the spend
ledger venture), the environment prefix (`RIGHTKIT` by default; `RIGHTKIT_*` names are always read
as a fallback) and the content subfolder for local outputs. `Context::with_db_file_name` names the
job-store database file (default `media-gen.sqlite3`).

Crate-specific literals go through `host::configure_media(MediaHostConfig { .. })`: the display name
used in user-visible messages (`app_name`), the stderr log prefix, the `User-Agent` of stock
downloads and the name of the spend-ledger attach lock file. The defaults are neutral. Environment
overrides (budget, Blender, Wan2GP, lip-sync model folder) are read as `<PREFIX>_NAME`
first, then `RIGHTKIT_NAME`.

The approval token domain is the host's (`host::approval::set_token_domain`, default
`rightkit-approval-v1`). The spend ledger's sidecar tables are `media_gen_spend_*`; a database with the
donor's `genright_spend_*` tables is renamed in place on first attach.

## Two ways in

- Typed (`typed`): `MediaGen::new(ctx)`, then `estimate`, `submit` (`GenerateRequest` to a job),
  `wait`/`status`, `result` (`Asset`s), `cancel`, `ack`.
- JSON commands (`api`, `media_commands`): what an engine CLI or app sidecar answers. Same path.

Container probing and the H.264 encoder rule are `rightkit-ffmpeg`'s (`quickprobe`, `h264_rule`).

## Asset-gated tests

- `providers::musicgen::tests::real_musicgen_produces_wav` (`#[ignore]`): MusicGen ONNX models and ONNX Runtime.
- `providers::local_lipsync::tests::wav2lip_end_to_end_renders_real_mp4` (`#[ignore]`): Wav2Lip/YuNet models, ONNX Runtime, ffmpeg.
- `providers::realesrgan::tests::real_binary_upscales_a_png_when_installed`: prints SKIP unless `RIGHTKIT_REALESRGAN_TOOLS` names an install.
