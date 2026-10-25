# rightkit-contracts 0.1.7 registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-contracts =0.1.7` exactly with the default feature set (`schema`), the set the PostRight engine uses. It installs the crate from the sealed archive through the offline directory registry and runs `src/main.rs`, which exits non-zero on the first failed assertion.

## What it proves

- The contract table. `Contract::ALL` lists the seven contracts in a fixed order. `name` and `from_name` round-trip, and an unknown or empty name is `None`. Every embedded schema is Draft 2020-12 JSON whose `$id` equals `schema_id()` and whose `version` equals `supported_version()`. `has_semantic_rules` is false for `channel-memory` only.
- The schemas' own examples. Every schema that declares `examples` has each example accepted by `validate` and by `check_typed`; only `channel-memory` declares none.
- Versions. `Version::parse` accepts strict `major.minor.patch` and refuses eleven malformed texts. `Version` orders numerically and prints as `1.2.3`. `negotiate` accepts the same major with an older or equal minor and any patch, and refuses another major (`IncompatibleMajor`), a newer minor (`NewerMinor`) and malformed text (`InvalidVersion`); `is_version_refusal` is true for these and for `MissingVersion`.
- Validation order. `validate` refuses a wrong contract name (`WrongContract`), a missing `contractVersion` (`MissingVersion`) and an incompatible major before any schema work, and a compatible but empty document reaches the schema stage (`Schema`).
- Typed documents. `validate_str` returns the document, `parse` returns a typed `ContentBrief` and `DeliveryEnvelope` with the expected fields, a typed value survives a JSON round trip without `null`, and text that is not JSON is `ContractError::Parse`.
- Failures. A schema error names the failing path, a carousel with a gap in its slide indexes is a separate `Semantic` error, a bad content hash is a `Schema` error, and `Display` on a `ContractError` names the contract.
- The eyes gate. `eyes_gate` for every `ReviewMode` and every combination of person approval and clean verdict, `EyesGate::name` for the three wire names, and `EYES_APPROVAL_REQUIRED`.

## What it deliberately does not prove

- The `fixtures` module and the `rightkit-contracts` command line binary. Both read a fixture tree from disk, and the sealed archive does not ship the fixtures.
- Every field of every typed struct. It reads the fields a content brief and a delivery envelope need, and the other typed contracts only through the schemas' own examples.
- The schema bytes beyond their `$id`, `version` and examples.
- Building with `default-features = false`, which drops `validate`, `parse` and `check_typed`; the apps use the default set.

State: in-memory strings only. No network, no files, no process spawn, no user data.
