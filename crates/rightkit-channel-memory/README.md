# rightkit-channel-memory

Channel memory for the content pipeline: the `channel-memory.v1` schema with serde types, validation, a version gate, proof citation rules, and scope checks for PostRight P3 and GenRight G7.

- Schema and types (`ChannelMemory`, `Scope`, `Proof`, `Lesson`, `IdentityBindings`, and related types). The JSON Schema is embedded at compile time (`SCHEMA_SOURCE`).
- Validation (`parse`). The order is: contract name and version, then the JSON Schema, then the typed model, then semantic rules (revision ordering, unique ids, date order).
- Scope (`ChannelMemory::ensure_scope`). A lookup for another venture, channel, or account is refused (`MemoryError::ForeignScope`).
- Proof citation (`ChannelMemory::cite_proof`, `approved_proofs`). A proof is cited only with its exact value and units. Observations never count as proofs.

Catalog capability: `channel-memory`. The catalog entry is declaration-only (`rightkit-ownership.json`). The revisioned daemon storage and conflict handling belong to the daemon (RK10). This crate validates records only.

The schema is still pending. It lives in `rightkit/contracts-pending/channel-memory/` until RK6 is accepted into `contracts/`.

## When to use it

- Use it to parse and validate a channel memory record before a PostRight P3 or GenRight G7 consumer reads it.
- Use it to check that a lookup matches the scope it was made for, and that a proof citation matches its approved value.

Do not use it to store or sync memory. Storage, revisions, and conflicts belong to the daemon. Do not use it to create proofs. A proof is personal and true. The crate only checks that a citation matches a recorded proof.

## Cargo features

None. The crate has no `[features]` table.

## Platform support

Pure Rust with no platform-specific code. It reads a JSON string and needs no OS services. The schema check uses `jsonschema` without default features.

## Usage

Assembled from the calls in `tests/consumers.rs`:

```rust
use rightkit_channel_memory::{parse, MemoryError, Scope};

fn check(text: &str) -> Result<(), MemoryError> {
    let memory = parse(text)?;
    memory.ensure_scope(&Scope {
        venture: "orthic".into(),
        channel: "legion-notes".into(),
        account: "main".into(),
    })?;
    let proof = memory.cite_proof("proof.receipt-launch-week", 1240.0, "USD")?;
    assert_eq!(
        proof.permitted_claims,
        vec!["Made $1,240 in launch week.".to_string()]
    );
    Ok(())
}
```

The test runs this against `contracts-pending/channel-memory/fixtures/valid/proof-bank-rev2.json`. A lookup for `kids-cartoon` returns `ForeignScope`, and a value of 1420.0 returns `ProofMismatch`.

## Tests

- `tests/fixtures.rs`: every fixture lands in the stage its name promises (schema, semantic, or version).
- `tests/consumers.rs`: scope isolation, proof citation, and the rule that website observations never become facts.

## Version

Crate version: see [INDEX.md](../../INDEX.md). The crate is not published (`publish = false`). It has no CHANGELOG.
