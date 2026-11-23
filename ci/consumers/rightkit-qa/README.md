# rightkit-qa 0.10.10 registry consumer (PK14)

Standalone consumer, outside the workspace, pinning `rightkit-qa =0.10.10` exactly with
`credential-owner` and `in-app-shot`. It exercises run-kind parsing and the scenario capture and
tier rules, `QaConfig::parse` on a committed sample, `Settings` parsing and typed reads, `check`
over a JSON result, the digest, base64 and secret-leak helpers, screenshot argument validation,
and credential owner configuration rejecting a traversal path before any native store access.

State: in-memory values only. No file, network, process, browser, window, FFmpeg or real user data.
The `control`, `browser`, `android` and `wv2-*` features are not enabled.

Not covered: the `control` and `embedded` surfaces, `in_app`, `browser`, `android`, run and
retention paths on disk, scenario execution, installed credential owner authentication and store
access, the media and loudness validators, and report writing.
This recipe must be revisited if the PK14 public API changes.

Run only by the CI consumer lane.
