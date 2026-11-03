# Changelog

## 0.1.1 (unpublished)

- Added the `rk-pair/1` short-authenticated-string functions to `pairing` (#965): `pairing_commit`, `pairing_code`, `label_bytes`, `NONCE_BYTES`, `MAX_LABEL_CHARS`, `CODE_DIGITS` (also re-exported at the crate root). Byte-identical to `rightkit-daemon`'s `remote/pairing.rs` and the `rightkit-sync-keys` copy it replaces. `rightkit-sync-keys` now consumes them and its `pairing_sas.rs` copy is deleted. The daemon's `remote/pairing.rs` is to delegate to this module (rightkit-daemon is owned by another batch). Not compiled here.

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (2026-10-17)

- New crate 0.1.0. `build_info` (`BuildIdentity`, `build_identity!`, runtime override, `build_script::emit`; Orthic-Labs/rightkit#503), `installation` (`InstallationStore`, `InstallationIdentity`, `StartupClaim`, clone detection by machine binding, host display name; #420), `pairing` (signed-transcript confirmation, owner-approval state machine, resume tickets, per-device grants, `ReplayStore`; #409), `bearer` (`BearerRegistry`, `RateLimiter`; #379).
- HMAC-SHA256, constant-time compare and hex live in-crate (`// TODO(rightkit-crypto)`) until rightkit-crypto has a locked version.
- Not compiled or run when written; verification is the CI run.
- Identity and claim transactions hold an exclusive file lock; `startup_generation` is append-only and a claim with a foreign generation quarantines the claim and re-mints (`ClaimOutcome::CloneDetected`); `release_claim_nonce`. Rate limiter keeps unrefilled buckets (shared overflow bucket) and folds IPv4-mapped IPv6 to IPv4. Build script watches the resolved git HEAD ref, packed-refs, index and sources. `host_display_name` keeps tab/newline as spaces.
