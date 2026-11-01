// Browser-QA mock for the native licence, legal-receipt and update commands that
// `rightkit-tauri-shell` answers in the real app. The views are the closed shapes the shared clients
// validate (`@rightkit/license` LicenseView, `@rightkit/app-update/native` views). Nothing real is
// activated, accepted, downloaded or installed; state lives for the page session.

import type { LicenseView } from "@rightkit/license";
import { LEGAL_MANIFEST_SHA256, REQUIRED_LEGAL_ACCEPTANCE_VERSION } from "../legal";
import type { InvokeArgs } from "./qaMockTypes";

interface QaReceipt {
  version: string;
  documentsHash: string;
  acceptedAt: string;
  eligibilityBasis: string;
}

// QA defaults carry an already-current receipt so ordinary browser QA flows land straight in the
// mail workspace, not the first-run acceptance screen. A test that wants the legal screen resets
// it with the "qa_reset_legal_acceptance" command.
let receipt: QaReceipt | null = {
  version: REQUIRED_LEGAL_ACCEPTANCE_VERSION,
  documentsHash: LEGAL_MANIFEST_SHA256,
  acceptedAt: "2026-07-16T00:00:00.000Z",
  eligibilityBasis: "individual",
};

const FREE: LicenseView = {
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
};

const PRO: LicenseView = {
  ...FREE,
  status: "active",
  lifecycle: "active",
  tier: "pro",
  kind: "term",
  stage: "time_bound",
  validUntil: Date.parse("2030-01-01T00:00:00.000Z"),
  licenseIdSuffix: "QA01",
  revisionState: "active",
};

let view: LicenseView = FREE;

function declared(args?: InvokeArgs): Record<string, unknown> {
  const request = args?.request;
  return request && typeof request === "object" ? (request as Record<string, unknown>) : {};
}

export function qaLicenseInvoke(
  command: string,
  args?: InvokeArgs,
): { handled: boolean; value: unknown } {
  switch (command) {
    case "rightkit_legal_receipt_read":
      return { handled: true, value: receipt };
    case "rightkit_legal_receipt_create": {
      const request = declared(args);
      receipt = {
        version: String(request.version ?? REQUIRED_LEGAL_ACCEPTANCE_VERSION),
        documentsHash: String(request.documentsHash ?? LEGAL_MANIFEST_SHA256),
        acceptedAt: new Date().toISOString(),
        eligibilityBasis: String(request.eligibilityBasis ?? "individual"),
      };
      return { handled: true, value: receipt };
    }
    case "rightkit_legal_receipt_reset":
    case "qa_reset_legal_acceptance":
      receipt = null;
      return { handled: true, value: command === "rightkit_legal_receipt_reset" ? null : undefined };
    case "rightkit_license_status":
      return { handled: true, value: view };
    case "rightkit_license_activate":
      view = PRO;
      return { handled: true, value: view };
    case "rightkit_license_refresh":
      return {
        handled: true,
        value: { view, outcome: view.status === "active" ? "refreshed" : "noCredential" },
      };
    case "rightkit_license_deactivate":
      view = { ...FREE, revisionState: "tombstone" };
      return { handled: true, value: view };
    case "rightkit_license_request_trial":
      return { handled: true, value: { accepted: true, message: "trialRequested" } };
    case "rightkit_update_check":
      return {
        handled: true,
        value: {
          available: false,
          eligible: false,
          refusal: "none",
          checkedAt: Date.now(),
          fresh: true,
        },
      };
    case "rightkit_update_status":
      return { handled: true, value: { state: "idle", message: "none" } };
    default:
      return { handled: false, value: undefined };
  }
}
