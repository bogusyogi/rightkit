# Changelog

## Unreleased

- Draft approvals (`issue`, `gate`, `consume`, `approval_fields`) come from `rightkit-media-gen-approvals` 0.1.0, where the GenRight approvals moved when `rightkit-approvals` 0.2.0 became the one approvals core (decision 3 of the 2026-11-11 shape review, taken on the recommended option, pending the owner's confirmation). Same calls, same records; a challenge issued before the move is not read (the app asks for a new one).
- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from GenRight (genright-studio recipes, contracts, catalog, characters, brands, kdp, video and swap templates).
