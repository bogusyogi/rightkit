# Changelog

## 0.1.5

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
