# rightkit-transcript-library 0.1.6 registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-transcript-library` to the exact version in its
`Cargo.toml`. Through the public API only, it proves: a draft record upserts into the in-memory
history store under its deterministic draft id, the default history query hides drafts,
`host_of_url` extracts a lower-cased host, and public history export formats an empty history in
memory.

The `store` and `managed-store` features are enabled. The consumer checks the unmanaged connection,
managed connection and key rotation, and file-backed history signatures without opening a database
or writing files. Other checks use in-memory values only. Run only by the CI consumer lane.
