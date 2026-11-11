# rightkit-memory 0.2.0 registry consumer

Standalone package that depends on `rightkit-memory` by exact registry pin (`=0.2.0`) outside the RightKit workspace and
exercises the memory engine through the one crate: tier names and ranks, token estimates and the hash embedder from the
core (crate root), and the SQLite memory store (`store`) opened in memory. Each passing check prints an `ok:` line; a failure
exits 1. CodeRight's memory client, published as `rightkit-memory` until 0.1.1, has its own consumer under
`rightkit-memory-client`.
