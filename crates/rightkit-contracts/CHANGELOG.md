# Changelog

## 0.1.7

- **`channel-memory` registered (#332).** New `Contract::ChannelMemory` (name `channel-memory`, `$id` `urn:rightkit:contract:channel-memory:v1`, supported version 1.0.0), listed in `Contract::ALL`, with the schema copied into `schemas/` (moved from `contracts-pending/channel-memory/` to `contracts/`) and checked by `tests/schema_copy.rs`. A downstream exhaustive `match` on `Contract` needs the new arm.
- Scope of the registration: schema validation and version negotiation only. `types::ChannelMemory` is an untyped wrapper over the JSON value; the typed model and the semantic rules (proof, conflict and supersession rules) stay in `rightkit-channel-memory`. New `Contract::has_semantic_rules()` is false for it, and `fixtures::check_tree` skips its `semantic-invalid/` category (those fixtures are run by `rightkit-channel-memory/tests/fixtures.rs`).
- Fixtures moved to `contracts/fixtures/channel-memory/`. The schema has no `examples`, so `every_schema_declares_examples_and_they_validate` skips it.
- Tests: `channel_memory_is_registered_with_schema_and_negotiation` in `tests/contracts.rs`; the shared fixture tests now cover the valid, invalid and version-rejected channel-memory fixtures.
- Not compiled or run when written; verification is the CI run.

## 0.1.6

- **`content-brief` 1.2.0 and `delivery-envelope` 1.2.0 (minor, additive; Orthic-Labs/rightkit#335).** Both schemas gain an optional `ruleRevisionRef`, a string `<platform>:<hash>` (platform id, then 32 lowercase hex) matching the `Display` form of `rightkit-platform-rules` `RuleRevision`. The integer `ruleRevision` and `policyRevision` are unchanged and stay distinct. Documents at 1.0.0 and 1.1.0 remain valid; a 1.3.0 document is refused as a newer minor (the `minor-ahead` fixtures moved from 1.2.0 to 1.3.0).
- **Rust.** `ContentBrief` and `DeliveryEnvelope` gain `rule_revision_ref: Option<String>` (omitted when `None`); struct literals need `rule_revision_ref: None`. `Contract::supported_version` returns 1.2.0 for both. The crate does not depend on `rightkit-platform-rules`; the field is the string form, and a consumer builds it with `RuleRevision::to_string()`.
- **Tests.** New fixtures `valid/rule-revision-ref.json` and `invalid/rule-revision-ref-malformed.json` for each contract; `typed_roundtrip.rs` sets the field in the all-optionals values; `contracts.rs` reads it through the typed views and checks a 1.1.0 envelope has none.
- Not compiled or run when written; verification is the CI run.

## 0.1.5

- **Eyes gate defined (#312).** New `review` module: `EYES_APPROVAL_REQUIRED` (`"eyes_approval_required"`), `EyesGate` (`approved`, `auto_clean`, `not_required`) and `eyes_gate(mode, person_approved, clean_verdict)`. The `content-brief` `review.mode` schema description now defines `manual`, `auto_when_clean` (clean `rightkit-review` verdict on the exact assets under `policyRevision`, else treated as `manual`, fail closed) and `none`. Description-only schema change, no version bump.

- **`daemon-api` 1.1.0 (minor, additive).** New optional auth class `device_call` (`POST /v1/call/{method}`, the transport-neutral request contract of `rightkit-daemon`): `authClasses.device_call` is optional, a route may use `authClass: "device_call"` (POST, `/v1/call/{name}`, no compare-and-swap), and a `device_call` route may mutate. `Contract::supported_version` returns 1.1.0 for `daemon-api`. `types::daemon_api::AuthClasses` gains `device_call: Option<AuthClass>` (struct literals need `device_call: None`) and `AuthClassId` gains `DeviceCall` (a downstream exhaustive `match` needs the new arm). Documents at 1.0.0 stay valid; new fixture `valid/api-v1-1-device-call.json`.

- **Publishable.** `publish = false` is removed. The six schemas are copied into the crate (`schemas/`) and embedded from there, so `cargo package` ships them; `tests/schema_copy.rs` checks each copy against `contracts/`. The manifest gains `readme`, `keywords`, `categories` and `include`, and the crate carries `LICENSE-MIT` and `LICENSE-APACHE`.
- **Optional fields are omitted, not written as `null`.** Every `Option` field on the typed structs in `types` now carries `skip_serializing_if = "Option::is_none"`, so a typed value serialises to JSON that the crate's schemas accept. Before, an absent optional such as `ruleRevision`, `copy`, `audio`, `captions`, `thumbnail` or `loudnessLufs` was written as `null`, which the schemas reject. The schemas are unchanged, and an explicit `null` still deserialises to `None`.
- **`content-brief` `supersedes`** is omitted when `None`. Revision 1 must not carry the key, so a written `null` failed validation there. Revisions 2 and later accept the key with a value or without it.
- **`daemon-api` exchange `body`** fields are unchanged. Their schema is `{}`, which accepts `null`.
- **Tests.** `tests/typed_roundtrip.rs` serialises a minimal value and an all-optionals-set value for each typed contract, asserts that the JSON text has no `null`, validates it, and reads it back to the same value.

## 0.1.4

- **`content-brief` 1.1.0 and `delivery-envelope` 1.1.0 (minor, additive).** Both schemas gain an optional `ruleRevision` (integer, at least 1), the platform rule revision the document was validated against. `policyRevision` on the envelope and `review.policyRevision` on the brief stay as they were; their descriptions now say they are review policy revisions, not rule revisions.
- **Brief facts** accept `withdrawn` and `superseded` as `status`. A `superseded` fact requires `supersededBy` (an id), enforced by a schema `if`/`then`. `withdrawn` needs no extra field.
- **Brief stages** accept `acquisition`, `transcription`, `labelling` and `playbook` as `id` and in `dependsOn`. Each has the same `requestId` and `inputHash` shape as the existing stages.
- **Rust.** `ContentBrief` gains `rule_revision`. `Fact` gains `superseded_by`. `FactStatus` gains `Withdrawn` and `Superseded`. `StageId` gains `Acquisition`, `Transcription`, `Labelling` and `Playbook`. `DeliveryEnvelope` gains `rule_revision`. `Contract::supported_version` returns 1.1.0 for both contracts. A downstream `match` on `FactStatus` or `StageId` that lists every variant needs the new arms.
- Documents at `1.0.0` remain valid. A `1.2.0` document is refused as a newer minor.

## 0.1.3

- **`schema` feature (default on).** `jsonschema` is now optional behind `schema`. It gates `validate`, `validate_str`, `parse`, `check_typed`, the `fixtures` module, the semantic cross-field rules and the `rightkit-contracts` binary. With `default-features = false` the typed structs in `types`, `Contract`, `ContractError` and `negotiate`/`Version` stay available and the crate no longer builds `jsonschema`. Default builds are unchanged.
