# Changelog

## 0.1.3

- **`schema` feature (default on).** `jsonschema` is now optional behind `schema`. It gates `validate`, `validate_str`, `parse`, `check_typed`, the `fixtures` module, the semantic cross-field rules and the `rightkit-contracts` binary. With `default-features = false` the typed structs in `types`, `Contract`, `ContractError` and `negotiate`/`Version` stay available and the crate no longer builds `jsonschema`. Default builds are unchanged.
