# rightkit-local-gen

Keyless local image generation: managed sd.cpp server runtime, memory modes, model profiles, durable job store and artifacts

Moved into RightKit from `genright-local-image` (genright) on 2026-11-01.

## Host configuration

The crate carries no product names. Call `rightkit_local_gen::host::configure` once at startup to set
the settings namespace, the model-store app label, the environment prefix (`RIGHTKIT` by default;
`RIGHTKIT_*` names are always read as a fallback) and the per-machine lock folder name.

Environment overrides (`<PREFIX>_` + suffix): `WORKSPACE_ROOT`, `CONTENT_ROOT`, `TOOLS_ROOT`,
`MODEL_ROOT`, `SD_SERVER`, `LOCAL_IMAGE_MEMORY_MODE`, `LOCAL_IMAGE_IDLE_SECONDS`, `HW_JSON`,
`MACHINE_LOCK_DIR`, `LOCAL_MODEL_WAIT_SECS`.
