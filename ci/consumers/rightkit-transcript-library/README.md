# rightkit-transcript-library registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-transcript-library` to the exact version in its
`Cargo.toml`. Through the public API only, it proves: a draft record upserts into the in-memory
history store under its deterministic draft id, the default history query hides drafts, and
`host_of_url` extracts a lower-cased host.

State: in-memory values only. No network, no files, no process spawn. Run only by the CI consumer
lane.
