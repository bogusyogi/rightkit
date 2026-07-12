# @rightkit/updates

Small runtime update-policy helper for Right Suite Tauri apps.

It does not build, sign, publish, or upload artifacts. `@rightkit/release` owns
that lane. This package runs inside apps and coordinates:

- checking the app updater;
- classifying `patch` vs `update`;
- auto-installing only public/free patches by default;
- deferring installs while the host app says it is unsafe;
- reporting status through host callbacks.

The package has no direct Tauri dependency. Apps inject their updater `check`
function, license-header loader, tier lookup, and safe-to-install predicate.
