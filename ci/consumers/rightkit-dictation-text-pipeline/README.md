# rightkit-dictation-text-pipeline registry consumer

Standalone consumer pinning `rightkit-dictation-text-pipeline` to the exact version in its
`Cargo.toml`. Through the public API only, it proves: the product vocabulary is empty until the
host configures it, then both term lists read back. A read-only settings store
preserves the file, repairs malformed fields in memory & follows external edits.
The first read from a changed data root keeps known settings while the file cannot be opened.
After the failure clears, a missing file reads defaults & a valid file replaces them.
Native runtime configuration takes precedence over disk settings.

The fixture uses isolated settings directories. Run only by the CI consumer lane.
