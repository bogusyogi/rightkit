# Vendored: citationberg 0.7.0

- Origin: crates.io `citationberg` 0.7.0 (`https://crates.io/api/v1/crates/citationberg/0.7.0/download`), upstream repository `https://github.com/typst/citationberg`, commit `25c66d5f79d919fb8446ef33075a70977066e82c` (from the crate's `.cargo_vcs_info.json`).
- Licence: MIT OR Apache-2.0 (`LICENSE-MIT`, `LICENSE-APACHE`, `NOTICE` kept unchanged).
- Omitted from the published crate: `Cargo.lock`, `.cargo_vcs_info.json` (the workspace lock governs).

## Changes from upstream

1. `Cargo.toml` and `Cargo.toml.orig`: `quick-xml` requirement `0.38.1` becomes `0.42.0` (features `serialize`, `overlapped-lists` unchanged).
2. No source changes. citationberg uses only `quick_xml::de::{Deserializer, SliceReader, DeError}`, `Deserializer::from_str`, `Deserializer::event_buffer_size`, `quick_xml::se::{Serializer::with_root, SeError}`; all exist unchanged in 0.42.0.

The version stays `0.7.0` so the workspace `[patch.crates-io]` entry applies.

## Advisories fixed

- RUSTSEC-2026-0194: quadratic duplicate-attribute check (quick-xml fixed in 0.41.0).
- RUSTSEC-2026-0195: unbounded `NsReader` namespace allocation (quick-xml fixed in 0.41.0).

## Behaviour changes inherited from quick-xml 0.42

- The serde `Deserializer` now enforces a recursion limit of 128 (`DeError::TooDeeplyNested`); upstream citationberg had none.
- The serde serializer escapes `\r`, `\n`, `\t` in attribute values and `\r` in text.
- The serde `Deserializer` namespace handling was reworked (quick-xml #953); `xml:lang` attributes must still round-trip (covered by the bundled tests).

## Removal

Remove when upstream citationberg releases with quick-xml >=0.41: delete this directory, the `[patch.crates-io]` entry and the `vendor/citationberg` workspace exclude in the root `Cargo.toml`.
