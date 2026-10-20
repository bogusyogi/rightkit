# rightkit-qa 0.8.0 registry consumer (PK14)

Standalone consumer, outside the workspace, pinning `rightkit-qa =0.8.0` exactly with default
features only. It exercises the pure public surface: run-kind parsing and the scenario capture and
tier rules, `QaConfig::parse` on a committed sample, `Settings` parsing and typed reads, `check`
over a JSON result, and the digest, base64 and secret-leak helpers.

State: in-memory values only. No file, network, process, browser, window, FFmpeg or real user data.
The `control`, `browser`, `android` and `wv2-*` features are not enabled.

Not covered: the `control` and `embedded` surfaces, `in_app`, `browser`, `android`, run and
retention paths on disk, scenario execution, the media and loudness validators, and report writing.
This recipe must be revisited if the PK14 public API changes.

Run only by the CI consumer lane.
