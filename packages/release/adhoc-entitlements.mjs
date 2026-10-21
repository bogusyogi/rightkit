// Ad hoc signatures cannot carry profile-restricted entitlements (iCloud, keychain-access-groups, application-identifier):
// AMFI would kill the app at launch. Ad hoc mode keeps only the hardened-runtime relaxations and audio input.
// Dependency-free so the QA ad hoc signer and development signing share one reduction.
export const isAdhocSafeEntitlement = (key) => key.startsWith("com.apple.security.cs.") || key === "com.apple.security.device.audio-input";

export function adhocSafeEntitlements(entitlements) {
  return Object.fromEntries(Object.entries(entitlements ?? {}).filter(([key]) => isAdhocSafeEntitlement(key)));
}
