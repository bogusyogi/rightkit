# Changelog

## 0.1.2

- New `compound` module (VO-2, `docs/contracts/voice.md` in the Pulse repository): `plan_compound` turns "open Claude and go to the first chat" into steps (a recognised command, an app launch, or an ordinal target from `parse_target`), only when every part is understood and the whole is not one catalog command; `run_plan` runs them against a `CompoundHost`. Ordinal targets are resolved by a host-supplied `TargetResolver`, read back, and only then selected; a target that is not found, ambiguous or unverified stops the plan at that step. No UI knowledge in the crate. Additive.

## 0.1.1 (unpublished)

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from HeardRight.
