# RkE2E Third-Party Notices

Build provenance: RkE2E 0.1.0, effective 2026-10-24.
inputs: pnpm-lock.yaml sha256 cae4f607c9647a6f9279e5ddf38377551816488061adbfb3b900d8dbf5989fb3; Cargo.lock sha256 0b25adb4baa7566fa30b9ac7aa89f4769c05794c2e0a45fceca2810f94a1d473; legal/THIRD_PARTY_NPM_LICENSES.html sha256 e02575c9e1889c02598c301b2fd3ea7731e3befe9b425b5192e5957d3f9dca71.
generated-by=@rightkit/legal legal-snapshot from legal/NOTICES.template.md

RkE2E includes third-party software that stays under its own licences. This notice does not reduce any permission those licences grant.

## npm packages

The production npm packages bundled into the RkE2E interface, with their licence texts, are in the appendix THIRD_PARTY_NPM_LICENSES.html (sha256 above). RightKit packages (`@rightkit/*`) are Damned Ventures LLC software under the Right Suite licence documents and are not listed there.

## Rust crates

The Rust crates linked into the RkE2E program are exactly those locked by Cargo.lock (sha256 above); each crate's licence is declared in its manifest on crates.io.
