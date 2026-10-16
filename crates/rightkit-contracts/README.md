# rightkit-contracts

Versioned contracts for the Right Suite content pipeline (CutRight, GenRight, PostRight), the recorder, and the daemon. The crate embeds six JSON Schema (Draft 2020-12) files, validates documents against them, runs the semantic checks a schema cannot express, deserializes into typed structs, and negotiates major and minor versions.

Catalog capability: `content-contracts`. Publishable on crates.io (wave 2.8); the six schemas ship inside the crate under `schemas/`.

## What it owns

| Contract | Schema file (in `rightkit/contracts/`) | Supported version |
|---|---|---|
| `content-brief` | `content-brief.v1.schema.json` | 1.1.0 |
| `delivery-envelope` | `delivery-envelope.v1.schema.json` | 1.1.0 |
| `record-session` | `record-session.v1.schema.json` | 1.2.0 |
| `record-event` | `record-event.v1.schema.json` | 1.2.0 |
| `demo-plan` | `demo-plan.v1.schema.json` | 1.0.0 |
| `daemon-api` | `daemon-api.v1.schema.json` | 1.0.0 |

The schemas are embedded with `include_str!`, so every installed engine validates against the same bytes. The crate's own schema and fixture tests check that each embedded schema is Draft 2020-12, has its `$id` and declares a version.

## Pipeline

1. `negotiate(contract, offered)` (re-exported from the private `version` module, `src/lib.rs:39`) reads the document's contract version and rejects an incompatible major, or a minor newer than this build knows, before any schema work.
2. The document is validated against the embedded schema (`validate`, `validate_str`).
3. Semantic rules that JSON Schema cannot express (unique ids, resolvable references, contiguous time maps) run in `semantic`.
4. `parse` and `check_typed` additionally deserialize into the structs in `types`.

Version policy: a document with major `M` and minor `m` is accepted by a build that supports `M.n` when `m <= n`. Patch numbers never gate acceptance. A different major is rejected, and so is a newer minor.

## When to use it

- Use it to validate a pipeline document or a daemon request before an engine acts on it.
- Use it to check a fixture tree in CI.

Do not use it to define product behaviour. The schemas are shared contracts; product rules belong to the owning app.

## Cargo features

`schema` (default): JSON Schema validation (`validate`, `validate_str`, `parse`, `check_typed`, `fixtures`), the semantic cross-field rules, the `rightkit-contracts` CLI and the `jsonschema` dependency. `jsonschema` 0.58.6 is used with its own default features off, so the crate never resolves a remote URI or pulls in HTTP or TLS. With `default-features = false` the typed structs in `types`, `Contract`, `ContractError` and version negotiation (`negotiate`, `Version`) remain and the crate drops `jsonschema` and its tree.

## CLI

Binary `rightkit-contracts`, from `src/main.rs`:

```text
rightkit-contracts validate <contract> <file>
rightkit-contracts check-fixtures <fixtures-dir>
rightkit-contracts list
```

Exit codes: 0 success, 1 validation or fixture failure, 2 usage error.

## Usage

Condensed from the loop at `tests/contracts.rs:13-26` (names checked against that file):

```rust
use rightkit_contracts::{check_typed, validate, Contract, Version};

for contract in Contract::ALL {
    let schema: serde_json::Value = serde_json::from_str(contract.schema_source())?;
    assert_eq!(schema["$id"].as_str(), Some(contract.schema_id()));
}
```

## Platform support

Pure Rust. No platform-specific code.

## Version and changes

Version: see [INDEX.md](../../INDEX.md). The crate directory holds its own copy of each schema (`schemas/`); the repository `contracts/` directory is the shared source and `tests/schema_copy.rs` fails if they differ. Release notes: [CHANGELOG.md](CHANGELOG.md).

Minor changes in this crate:

- `record-event` 1.1.0 adds the event kinds `session_start`, `session_end` and `frame_dropped`, and the `frameDropped` object (`count`, `intervalMs`, `reason` one of `backpressure`, `encoder`, `capture_gap`, `unknown`), required for `frame_dropped`. 1.0.0 documents remain valid.
- `record-event` 1.2.0 accepts `hmac-sha256:<64 lowercase hex>` as a hash `digest` beside `blake3:<64 lowercase hex>`; `saltId` names the key (`hmac-sha256:<keyId>`). 1.0.0 and 1.1.0 documents remain valid.
- `record-session` 1.1.0 adds `on_screen_visible` to `capture.visibilityState`. 1.0.0 documents remain valid.
- `record-session` 1.2.0 adds the optional `capture.border` object (`required`, `access`, `error`): the OS capture border state a backend reports, absent when it reports none. 1.0.0 and 1.1.0 documents remain valid.
