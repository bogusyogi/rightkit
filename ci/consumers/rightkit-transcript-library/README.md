# rightkit-transcript-library 0.1.4 registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-transcript-library` to the exact version in its
`Cargo.toml`. Through the public API only, it proves: a draft record upserts into the in-memory
history store under its deterministic draft id, the default history query hides drafts,
`host_of_url` extracts a lower-cased host, and public history export formats an empty history in
memory.

The `store` feature is enabled; the consumer references `DeliveryHistoryStore::open_connection` to cover
the public API without opening a connection, running migrations, or touching user data. Other checks
use in-memory values only. No network, files, or process spawn. Run only by the CI consumer lane.
