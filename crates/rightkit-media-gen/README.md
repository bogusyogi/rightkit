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

On-disk schema ids that existing data depends on are kept unchanged: the `genright_spend_*` ledger
tables and the `genright-approval-v1` approval token domain.

## Asset-gated tests

- `providers::musicgen::tests::real_musicgen_produces_wav` (`#[ignore]`): MusicGen ONNX models and ONNX Runtime.
- `providers::local_lipsync::tests::wav2lip_end_to_end_renders_real_mp4` (`#[ignore]`): Wav2Lip/YuNet models, ONNX Runtime, ffmpeg.
- `providers::realesrgan::tests::real_binary_upscales_a_png_when_installed`: prints SKIP unless `RIGHTKIT_REALESRGAN_TOOLS` names an install.
