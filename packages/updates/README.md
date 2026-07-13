# @rightkit/updates

Small runtime update-policy helper for Right Suite Tauri apps.

It does not build, sign, publish, or upload artifacts. `@rightkit/release` owns
that lane. This package runs inside apps and coordinates:

- checking the app updater;
- classifying `patch` vs `update` directly from Tauri's exact `rawJson` manifest;
- auto-installing only public/free patches by default;
- deferring installs while the host app says it is unsafe;
- reporting status through host callbacks.
- loading version-only public release status so Free users can see a Pro-update nudge without receiving private artifact coordinates.

The package has no direct Tauri dependency. Apps inject their updater `check`
function, license-header loader, eligibility policy, and safe-to-install predicate.

Manifest `tier` is mandatory and must be exactly `patch` or `update`. Missing or
unknown values fail closed and clear any previously cached update handle. Apps
must use Tauri's returned `rawJson`; they must not probe a second manifest to
reconstruct the tier.
