import { createLicenseMock } from "@rightkit/license/testing";

// Browser-QA only (loaded through `browserQaLicenseClient()` in tauriBridge, behind BROWSER_QA_BUILD).
// The mock is in-memory: no persistence, credentials or network. The `testing` export resolves
// only under the `development` condition, so a release build must never reach this module.
export function qaLicenseClient() {
  return createLicenseMock().client;
}
