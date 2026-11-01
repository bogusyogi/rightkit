# RkE2E Third-Party Notices

Build provenance: RkE2E 0.1.0, effective 2026-10-24.
inputs: pnpm-lock.yaml sha256 2b7fa1608b78a637b9e25cfa3411f08cf934338663af724988b3ab9fca0d1764; Cargo.lock sha256 e6c1b9796fe3ae5ea8893eb7518e2e17c9496be85682c3af6633e6e522e06ef2; legal/THIRD_PARTY_NPM_LICENSES.html sha256 e38d74f8ea247a7012c434e5df5089f8c138f19a2a2a4a0214fefffbeede272f.
generated-by=@rightkit/legal legal-snapshot from legal/NOTICES.template.md

RkE2E includes third-party software that stays under its own licences. This notice does not reduce any permission those licences grant.

## npm packages

The production npm packages bundled into the RkE2E interface, with their licence texts, are in the appendix THIRD_PARTY_NPM_LICENSES.html (sha256 above). RightKit packages (`@rightkit/*`) are Damned Ventures LLC software under the Right Suite licence documents and are not listed there.

## Rust crates

The Rust crates linked into the RkE2E program are exactly those locked by Cargo.lock (sha256 above); each crate's licence is declared in its manifest on crates.io.
