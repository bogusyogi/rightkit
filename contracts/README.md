# RightKit contracts

JSON Schema (Draft 2020-12) files for the Right Suite content pipeline, recorder and daemon. `crates/rightkit-contracts` embeds each file with `include_str!`, validates documents against it, and deserialises them into typed structs. The crate README has the CLI, pipeline and version policy.

| File | Contract | Supported version |
|---|---|---|
| `content-brief.v1.schema.json` | content-brief | 1.1.0 |
| `delivery-envelope.v1.schema.json` | delivery-envelope | 1.1.0 |
| `record-session.v1.schema.json` | record-session | 1.2.0 |
| `record-event.v1.schema.json` | record-event | 1.2.0 |
| `demo-plan.v1.schema.json` | demo-plan | 1.0.0 |
| `daemon-api.v1.schema.json` | daemon-api | 1.0.0 |

## Versioning

- The file name carries the major version (`v1`), and the schema `$id` is `urn:rightkit:contract:<name>:v1`. A minor revision keeps both and changes the schema `version` and the supported version in `Contract::supported_version`.
- Minor revisions are additive. A new field is optional unless the table below says otherwise. Every document at an older minor stays valid.
- A document with a newer minor than the build supports is refused before schema validation (`NewerMinor`). A different major is refused the same way.
- Each minor revision ships with a fixture for the new fields. Valid fixtures live in `fixtures/<contract>/valid/`, invalid ones in `invalid/`, semantic failures in `semantic-invalid/` and version refusals in `version-rejected/`. `rightkit-contracts check-fixtures contracts/fixtures` runs the whole tree.

## Minor revisions

| Contract | Version | Change |
|---|---|---|
| content-brief | 1.1.0 | Optional `ruleRevision` (platform rule revision). Fact `status` adds `withdrawn` and `superseded`; `superseded` requires `supersededBy`. Stage `id` and `dependsOn` add `acquisition`, `transcription`, `labelling` and `playbook`. |
| delivery-envelope | 1.1.0 | Optional `ruleRevision`. `policyRevision` is the review policy revision, not the platform rule revision. |

Field meanings:

- `ruleRevision` is the platform rule revision a document was validated against.
- `review.policyRevision` (brief) and `policyRevision` (envelope) are review policy revisions.

Approval records are not defined in these schemas. A `ruleRevision` on an approval record needs a new contract; it is not added here.
