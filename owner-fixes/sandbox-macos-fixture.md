
## 2026-10-13 browser_fixture_profile flake (run 38425528198)
Cause: FixtureTree dir name used pid+nanos only; parallel tests on macOS got the same name (coarse clock), so one test's Drop deleted the other's tree (AlreadyExists at :36, NotFound at :62). Not a profile-code change. Fix: per-process atomic counter in the dir name; all security assertions unchanged.
