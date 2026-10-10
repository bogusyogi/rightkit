# Changelog

## 0.2.1

- **`support-bundle` feature (default on).** The `zip` dependency is now optional behind `support-bundle`, which gates `build_support_bundle`, `SupportBundleOptions`, `BundleManifest` and `create_zip` (about 200 lines). Panic capture, pending-report handoff, native capture and retention are unchanged without it. Default builds are unchanged.
