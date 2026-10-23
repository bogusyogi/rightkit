
## 2026-10-13 browser_fixture_profile flake (run 38425528198)
Cause: FixtureTree dir name used pid+nanos only; parallel tests on macOS got the same name (coarse clock), so one test's Drop deleted the other's tree (AlreadyExists at :36, NotFound at :62). Not a profile-code change. Fix: per-process atomic counter in the dir name; all security assertions unchanged.

## 2026-10-23 Chrome for Testing 156: `com.apple.CARenderServer` lookup (residual)
Cause: 156 exits 0 at startup, before its mach rendezvous server exists, without a lookup of `com.apple.CARenderServer` (children then abort "No rendezvous client"); 155 tolerated the denial. Fix: one exact `(allow mach-lookup (global-name "com.apple.CARenderServer"))` in `fixture_profile_for`; `com.apple.windowserver.active` also worked alone but is the broader WindowServer connection, so it is not granted. Verified 7/7 fixture proofs on 156.0.8078.47 and 155.0.8059.39.
Residual: a Core Animation (or WindowServer) connection lets the confined tree use TCC grants (Screen Recording, Accessibility) held by the responsible launcher process. Data still cannot leave except through the profile directory (the only write grant) and the loopback proxy port (the only network grant). Launch the fixture from a launcher that holds no such grants.
