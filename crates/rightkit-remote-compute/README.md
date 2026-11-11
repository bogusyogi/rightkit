# rightkit-remote-compute

Rented GPU workers: SSH and Vast.ai drivers, the register/stage/run/teardown lifecycle, and the
time-billed spend bracket around a rental (reserve before the create, settle at destroy, enforcement
of duration, budget and idle limits). `commands::handle` answers the rented-worker commands
(`studio.models.workers`, `rental.search`, `worker.action`, `rentals`, `rentals_enforce`).

Worker records are kept by a `WorkerStore`. The default (`SettingsWorkerStore`) is the media-gen job
database's settings table, beside the spend ledger the rental bracket reserves in; a host that keeps
workers elsewhere calls `set_worker_store` once at start.

Builds on `rightkit-media-gen` (job store, spend ledger with its daily, monthly and device limits,
credentials, HTTP policy). The rental label prefix, the Vast `client_id` and the default remote
workdir use the host settings namespace (`rightkit_media_gen::host::HostConfig::settings_namespace`),
so a host keeps finding the rentals it created before the move.

Local model management (catalogue, installs, governor) is `rightkit-media-gen-models`.

Moved into RightKit from `genright-studio` (`model_management/remote.rs`, `rental.rs` and the worker
record helpers of `model_management/mod.rs`) on 2026-11-01.
