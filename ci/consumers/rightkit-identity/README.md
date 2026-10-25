# rightkit-identity registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-identity` to the exact version in its
`Cargo.toml` with default features (the crate has none). It also depends on `serde_json = "1"`, which
is already in the crate's own dependency closure, to check the persisted snapshots. It is built from
the sealed archive through the offline directory registry. The only platform-gated check is the
owner-only file mode, which runs on Unix; every other assertion runs on Windows, macOS and Linux alike.

Every time-dependent call takes an explicit clock and the process-liveness probe is injected, so the
run uses no wall clock and probes no process. It proves:

- Build identity: `from_compile_time` defaults every unset value to `unknown` and reads the dirty
  flag from `1`, `true`, `yes` and `dirty` only; `with_runtime_override` applies each of the five
  `RIGHTKIT_*` names; `display`, the `build_identity!()` macro (it reports the consumer's own
  version), `utc_stamp`, and the serde round trip.
- Installation identity: `machine_binding_hash` equals an independently computed SHA-256;
  `host_display_name`; `InstallationStore` mints a fresh identity, resumes it, reports a clone with
  the previous id when the machine binding changes, writes owner-only on Unix, and reports a
  corrupt record; `claim_startup` is acquired, held by a live process, or an unclean previous exit,
  advances the startup generation, and quarantines a colliding claim after a snapshot rollback;
  `release_claim` and `release_claim_nonce` remove a claim only for the right holder; eight
  concurrent first runs agree on one id.
- Pairing: the canonical transcript encoding ignores effect order and duplicates; `transcript_mac`
  and `confirmation_code` equal independently computed HMAC-SHA256 values and the two roles differ;
  `PairingSession` refuses a reflected MAC, approval before confirmation, a grant wider than
  requested, reuse after completion, and use after denial or expiry; `ResumeTicket::issue`,
  `verify` and `redeem` refuse a wrong key, a tampered device or expiry, an expired ticket and a
  replay; `ReplayStore` is bounded, prunes expired ids and survives a serde snapshot.
- Bearer tokens: `BearerRegistry` mints a `rkb_` token, keeps only hashes, revokes the earlier token
  on a new mint, authenticates after a serialised round trip; `RateLimiter` is per IP, refills,
  shares a bucket per IPv6 /64 and per IPv4-mapped address, keeps an exhausted bucket under
  capacity churn, and spends a token before any comparison.

It does not prove `build_script::emit` (it belongs in a `build.rs` and runs `git`), the compile-time
`RIGHTKIT_*` values a real build supplies, a real process-liveness probe, cross-process file
locking, or key agreement (the caller supplies the shared secret).

State: a per-process directory under the OS temp directory, removed at the end. No network, no
process spawn, no wall clock, no real user data. Run only by the CI consumer lane.
