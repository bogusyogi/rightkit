# Rhook cutover review

97 catalog rows: 1 Y, 4 N, 0 M, 92 NA.

Rhook is a Rust CLI hook executor. One live optional route delegates heavy build guarding to `@rightkit/release`; source-first resolver path remains a delivery flag.

- `cargo:rightkit-fs` — replace local atomic state writes with `rightkit-fs`.
- `cargo:rightkit-db` — move AuthorityStore connection bootstrap to `rightkit-db`.
- `cargo:rightkit-process` — move compact and cortex child lifecycle to `rightkit-process`.
- `cargo:rightkit-sandbox` — map resource reservations to `rightkit-sandbox`, preserving signed-capsule policy.

No active Node or Swift runtime exists. `@rightkit/hooks` remains NA: Rhook's host-specific Rust policy does not duplicate that lifecycle runtime. Review is source-only; no builds, installs, network, or product edits ran.


