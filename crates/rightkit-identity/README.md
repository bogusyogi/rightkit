# rightkit-identity

Identity primitives for Right Suite apps. Not yet published (0.1.0).

- `build_info`: `BuildIdentity` (git rev, dirty flag, profile, channel, UTC stamp, version). Compile-time values come from `build_identity!()` (`option_env!` of `RIGHTKIT_GIT_REV`, `RIGHTKIT_GIT_DIRTY`, `RIGHTKIT_BUILD_PROFILE`, `RIGHTKIT_CHANNEL`, `RIGHTKIT_BUILD_UTC`); `with_runtime_override` applies the same variable names at run time. `build_script::emit()` prints those variables from a `build.rs`.
- `installation`: `InstallationStore` persists an `InstallationIdentity` with an atomic owner-only write. A changed machine binding is reported as `Startup::CloneDetected` and mints a new id. `claim_startup` / `release_claim` detect an unclean previous exit. `host_display_name` normalises a host name.
- `pairing`: canonical transcript encoding, role-separated HMAC confirmation, six-digit confirmation code, `PairingSession` owner-approval state machine, `DeviceGrant`, signed `ResumeTicket`, `ReplayStore`.
- `bearer`: `BearerRegistry` stores only token hashes, compares in constant time, revokes per device, and consults a per-IP `RateLimiter` token bucket before comparing.

Every time-dependent call takes the clock as an argument. Not implemented: transport, UI, cross-device key agreement (the caller supplies the shared secret), persistence of the bearer registry and replay store beyond `serde` snapshots.
