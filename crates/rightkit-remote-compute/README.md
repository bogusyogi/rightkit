# rightkit-remote-compute

Rented GPU workers: SSH and Vast.ai drivers, the register/stage/run/teardown lifecycle, and the
time-billed spend bracket around a rental (reserve before the create, settle at destroy, enforcement
of duration, budget and idle limits).

Moved into RightKit from `genright-studio` (`model_management/remote.rs`, `rental.rs` and the worker
record helpers of `model_management/mod.rs`) on 2026-11-01.

Builds on `rightkit-media-gen` (job store, spend ledger, credentials, HTTP policy). The rental label
prefix, the Vast `client_id` and the default remote workdir use the host settings namespace
(`rightkit_media_gen::host::HostConfig::settings_namespace`), so a host keeps finding the rentals it
created before the move. Environment seams (`test-mode` builds only): `RIGHTKIT_RENTED_DRIVER=local-fs`,
`RIGHTKIT_RENTED_DIR`, `RIGHTKIT_TEST_VAST_KEY_FILE`, `RIGHTKIT_TEST_NOW`.
