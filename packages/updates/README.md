# @rightkit/updates

`UPDATE_HOST_CONTRACT_VECTORS` is the framework-neutral free/Pro patch/update
matrix that every app adapter must execute before the Phase 3 publication gate.
It prevents app-local tests from silently redefining patch eligibility or the
Pro upgrade nudge.

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
function, authorization loaders, eligibility policy, and safe-to-install predicate.

## Check and install authorization

`getHeaders` is the manifest-check header loader. An app may separately provide
`getInstallHeaders(tier, { checkHeaders })` after the returned manifest
has an exact tier and the user is eligible. This lets a private Pro artifact use
a signed entitlement without sending that entitlement to the manifest endpoint:

```ts
createAutoUpdateClient({
  check: (options) => check(options),
  getHeaders: () => ({ Authorization: `Bearer ${activationToken}` }),
  getInstallHeaders: async (tier) => tier === 'update'
    ? { Authorization: `Bearer ${await loadSignedEntitlement()}` }
    : undefined,
});
```

The eligible update handle stores only the update, tier, check headers, and its
check generation. Install headers are resolved only after `canInstall` succeeds,
immediately before `downloadAndInstall`, and are never cached. The callback is
never called for an ineligible or malformed-tier update, and an exception fails
closed without downloading. Returning `undefined` supports header-free public
patches.

Compatibility warning: omitting `getInstallHeaders` reuses the `getHeaders`
result for installation, which may forward that credential to a different
artifact origin. Existing integrations retain that behavior, but new apps must
set `getInstallHeaders` explicitly (returning `undefined` for public patches).

Manifest `tier` is mandatory and must be exactly `patch` or `update`. Missing or
unknown values fail closed and clear any previously cached update handle. Apps
must use Tauri's returned `rawJson`; they must not probe a second manifest to
reconstruct the tier.
