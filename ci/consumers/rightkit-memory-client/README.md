# rightkit-memory-client registry consumer

Standalone package that depends on `rightkit-memory-client` by exact registry pin (`=0.1.0`) outside the RightKit workspace and
exercises a few cheap public API checks with no external assets: write screening, the scope slug, the membrane schema check,
and that the backend's tier is the engine's `rightkit-memory-core` type (wire names and ranks). Each passing check prints an `ok:`
line; a failure exits 1. (Until 0.1.1 this crate was published as `rightkit-memory`; that name now holds the memory engine.)
