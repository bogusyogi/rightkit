# rightkit-remote-compute 0.2.0 registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-remote-compute =0.2.0` exactly with its default features. It installs the crate from the sealed archive through the offline directory registry and runs `src/main.rs`, which prints `ok:` lines and exits 1 on the first failed check.

## What it proves

- `rental::rental_due` decides from a worker record alone: no limit is never due, a rental past `max_duration_min` is due for `Duration`, one past its reserved budget is due for `Budget`.
- `remote::rental_label` prefixes the worker id with the host settings namespace (`rightkit` by default), and `WORKERS_KEY` is the settings key of the worker records.
- 0.2.0: `commands::COMMANDS` holds the five rented-worker commands and no local model command (those moved to `rightkit-media-gen-models`), and `SettingsWorkerStore` is a `WorkerStore`.

## What it deliberately does not prove

Anything that needs external assets (model files, provider keys, network, ffmpeg or a desktop session). The crate's own tests cover those, asset-gated.
