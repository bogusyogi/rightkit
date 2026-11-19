# rightkit-dictation-text-pipeline registry consumer

Standalone consumer pinning `rightkit-dictation-text-pipeline` to the exact version in its
`Cargo.toml`. Through the public API only, it proves: the product vocabulary is empty until the
host configures it, then both term lists read back. A read-only settings store
preserves the file, repairs malformed fields in memory & follows external edits.
Native runtime configuration takes precedence over disk settings.

State is confined to a temporary settings file. Run only by the CI consumer lane.
