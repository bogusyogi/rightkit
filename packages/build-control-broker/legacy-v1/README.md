# Legacy v1 forensic archive

This directory is **not executable broker code**. It preserves v1 source & its
former tests solely for incident reconstruction during migration.

- `v1-broker-source.mjs.txt` — historic v1 broker source; SHA-256:
  `6a7c4c97bdba35b285e8fac97277c22aa49a90fe23d2316396e0ea85265fc0d2`.
- `v1-broker-tests.mjs.txt` — historic v1 tests; SHA-256:
  `73d9de327d15ea85fb7b20e28686b2e3a7701613a4fd86b09ea2293a9b9018b1`.

No live module imports this directory. `quarantine.test.mjs` asserts this
boundary & archive integrity; it does not execute archived behavior.
