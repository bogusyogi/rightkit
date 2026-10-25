# RightApp Third-Party Notices

Build provenance: RightApp {{version}}, effective {{effectiveDate}}.
inputs: pnpm-lock.yaml sha256 {{lock}}; Cargo.lock sha256 {{cargoLock}}; legal/THIRD_PARTY_NPM_LICENSES.html sha256 {{npmAppendix}}.
generated-by=@rightkit/legal legal-snapshot from legal/NOTICES.template.md

RightApp includes third-party software that stays under its own licences. This notice does not reduce any permission those licences grant.

## npm packages

The production npm packages bundled into the RightApp interface, with their licence texts, are in the appendix THIRD_PARTY_NPM_LICENSES.html (sha256 above). RightKit packages (`@rightkit/*`) are Damned Ventures LLC software under the Right Suite licence documents and are not listed there.

## Rust crates

The Rust crates linked into the RightApp program are exactly those locked by Cargo.lock (sha256 above); each crate's licence is declared in its manifest on crates.io.
