import type { LicenseView } from "@rightkit/license";

/** A `LicenseView` with every field set (nothing stored, unlicensed). `over` overrides any field. Test-only; excluded from the build. */
export function licenseView(over: Partial<LicenseView> = {}): LicenseView {
  return {
    mode: "configured",
    status: "none",
    lifecycle: "unlicensed",
    tier: null,
    features: [],
    kind: null,
    stage: null,
    validUntil: null,
    confirmationDueAt: null,
    staleSince: null,
    licenseIdSuffix: null,
    busy: false,
    unresolved: false,
    revisionState: "none",
    message: "none",
    ...over,
  };
}

/** A stored, active lifetime licence at the pro tier. */
export const LIFETIME_PRO: LicenseView = licenseView({
  status: "active",
  lifecycle: "lifetime",
  tier: "pro",
  kind: "lifetime",
  stage: "final",
  revisionState: "active",
  licenseIdSuffix: "0001",
});

/** A stored, active lifetime licence at the free tier: licensed, not pro. */
export const LIFETIME_FREE: LicenseView = licenseView({ ...LIFETIME_PRO, tier: "free" });

/** The view after a deactivation: nothing active, the stored revision is a tombstone. */
export const DEACTIVATED: LicenseView = licenseView({ revisionState: "tombstone" });
