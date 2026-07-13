# `@rightkit/qa`

Shared native QA primitives for Right Suite desktop apps. The package owns run locking, bearer-authenticated loopback control, deterministic evidence bundles, exact owned-process cleanup, and the guard that keeps development WebDriver access out of production builds.

This package is intentionally private until the Windows and macOS phase gate passes. App repositories must never consume it through a Git, path, `file:`, `link:`, or `workspace:` dependency; adoption happens only after a registry version is published.
