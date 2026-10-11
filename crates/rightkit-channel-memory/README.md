# rightkit-channel-memory

Channel memory for the content pipeline: the `channel-memory.v1` schema with serde types, validation, a version gate, proof citation rules, and scope checks for PostRight P3 and GenRight G7.

- Schema and types (`ChannelMemory`, `Scope`, `Proof`, `ConflictRecord`, `Lesson`, `IdentityBindings`, and related types). The JSON Schema is embedded at compile time (`SCHEMA_SOURCE`).
- Exact decimals (`Decimal`). A proof value is kept as text and compared as an exact decimal. No float, no tolerance.
- Validation (`parse`). The order is: contract name and version, then the JSON Schema, then the typed model, then semantic rules (revision ordering, unique ids, date order, claims that mirror their proof, conflict bodies, approved-scope agreement).
- Scope (`ChannelMemory::ensure_scope`). A lookup for another venture, channel, or account is refused with `ReasonCode::ForeignScope`.
- Proof citation (`ChannelMemory::cite_proof`, `approved_proofs`). A citation is checked in the order of contract §5.3 and returns the first failing `ReasonCode`: scope, observation, existence, status, revision, validity window, claim, value, units, subject, period, open conflict.
- Status transitions and revision chain (`check_transition`, `check_successor`, `check_stale_arrival`). Withdrawn and superseded are terminal, approved content is frozen, and an older revision cannot restore a withdrawn proof.
- Controlled writes (`check_write`, `WriteRequest`). A write carries author, intent and base revision. A skill cannot set a proof above collected or change authoritative memory. Observations cannot be authoritative.
- Record-level entry points for apps that store proofs themselves. `validate_proof(&Proof, &Scope)` checks one proof in isolation (schema, period and validity order, claim mirroring, unique claim ids). It shape-checks the scope argument only and does NOT prove the proof belongs to that scope: a `Proof` carries no scope, so a foreign scope is refused by `ensure_scope` at document level or by the caller. `check_proof_write(prev: Option<&Proof>, next: &Proof, author: Author)` checks one status change (author gate, status authority, transitions, record rules). Both use the same functions as `parse` and `check_write`. Rules that need sibling records stay document-level; see the CHANGELOG.
- Typed construction (`Decimal::try_parse`, `Unit::try_parse`). A consumer that builds records from its own storage calls these instead of serde. A malformed value or unit is refused with `E_NUMBER_INVALID` or `E_UNIT_INVALID`. Serde and the typed path accept and refuse the same text (one table in `tests/typed_construction.rs`).
- Approved-scope conflicts (`conflicts_with(a, b) -> bool`). True exactly when two approved proofs disagree for the same subject, period and units with different values. It compares `subject` and `units` as exact text, `period` as both exact dates, and `value` as an exact decimal. It ignores validity windows, observation dates, sources and claims, and a proof never conflicts with itself. A consumer that stores proofs itself calls it at approval time. The document path (`parse`) uses the same function and refuses with `E_PROOF_CONFLICT_SCOPE`.
- Supersession (`check_supersession(prev, successor)`). A superseded record and its successor agree: the record names the successor given, the ids differ, and the successor is approved. The document path runs the same rule, with the successor looked up in the document.
- Units (`Unit`). A built-in currency code, `percent`, `count`, `days` or `seconds`, or an open slug such as `views`, `subscribers`, `viewers_percent` or `shorts_per_week`. The reserved tokens are `_per_` and a trailing `_per`. A rate must end in `_per_day`, `_per_week`, `_per_month` or `_per_year`, and the part before it must be a plain base: no `_per_`, and not ending in `_per`. `x_per_hour` and `a_per_per_day` are refused.
- Banned moves (`BannedMove::matches`). A move matches text that contains one of its `phrases`, ignoring case.

Catalog capability: `channel-memory`. The catalog entry is declaration-only (`rightkit-ownership.json`). The revisioned daemon storage and conflict handling belong to the daemon (RK10). This crate validates records only.

The schema is still pending. It lives in `rightkit/contracts-pending/channel-memory/` until RK6 is accepted into `contracts/`.

## When to use it

- Use it to parse and validate a channel memory record before a PostRight P3 or GenRight G7 consumer reads it.
- Use it to check that a lookup matches the scope it was made for, and that a proof citation matches its approved value.

Do not use it to store or sync memory. Storage, CAS on the wire, and conflict creation belong to the daemon. The crate checks the records, transitions and writes it is given. Do not use it to create proofs. A proof is personal and true. The crate only checks that a citation matches a recorded proof.

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
    let citation = Citation {
        proof_id: "proof.receipt-launch-week".into(),
        proof_revision: 2,
        claim_id: "claim.launch-week-revenue".into(),
        value: Decimal::parse("1240.00").expect("decimal"),
        units: "USD".into(),
        subject: "subject.launch-week-revenue".into(),
        period: DateWindow { from: "2026-03-01".into(), to: "2026-03-07".into() },
    };
    let scope = Scope { venture: "orthic".into(), channel: "legion-notes".into(), account: "main".into() };
    let proof = memory.cite_proof(&scope, &citation, "2026-10-11")?;
    assert_eq!(proof.permitted_claims[0].text, "Made $1,240 in launch week.");
    Ok(())
}
```

The examples in `tests/` run against `contracts-pending/channel-memory/fixtures/`. A lookup for `kids-cartoon` returns `ForeignScope`, a citation of `1420.00` returns `E_NUMBER_MISMATCH`, and a withdrawn proof returns `E_PROOF_WITHDRAWN`.

## Tests

- `tests/fixtures.rs`: every fixture lands in the stage its name promises (schema, semantic, or version).
- `tests/consumers.rs`: scope isolation, the citation match for each field, validity windows, open conflicts, and the rule that website observations never become facts.
- `tests/proof_rules.rs`: status transitions, the revision chain, controlled writes by author and intent, conflict records, and banned-move phrases.
- `tests/record_rules.rs`: `validate_proof` and `check_proof_write` for each rule they cover, and the document and record paths returning the same error for the same defect.
- `src/unit.rs`: accepted and refused unit text, including the 32-character base limit.
- `src/decimal.rs`: exact decimal equality and the accepted text form.

Each rule has a passing and a failing case.

## Version

Crate version: see [INDEX.md](../../INDEX.md). The crate is not published (`publish = false`). Changes are recorded in [CHANGELOG.md](CHANGELOG.md).
