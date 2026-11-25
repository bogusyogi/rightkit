# Changelog

## 0.1.1

- Logic moved from Pulse into this crate (bogusyogi/pulse#643): see the module docs for what each new module holds.

## 0.1.0

- Depends on `rightkit-owner-approvals` 0.1.3 (was 0.1.0). Not compiled.
- Split out of `rightkit-mesh`: the `devicelink` module, with no transport dependency. `PathAggregate::observe` takes any `PathObservation`.
