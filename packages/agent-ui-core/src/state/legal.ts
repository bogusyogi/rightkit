// First-run legal acceptance for the Right Suite Desktop License. The contract (manifest shape,
// declaration, digest) is @rightkit/legal; the receipt itself is owned natively
// (`rightkit-tauri-shell` host legal service, reached through `@rightkit/onboarding`'s
// `createNativeLegalReceiptPort`): the webview declares what it showed, native code compares it
// with the bundled manifest, stamps the time and stores the record. This file is only the
// zustand wiring of that port (see `state/themeStore.ts`); it keeps no copy of the receipt.
//
// The acceptance version is the manifest's own `acceptanceVersion`, so a material change
// re-prompts: the native `read` answers no receipt for a changed version or digest.

import { acceptanceDeclaration, materialAcceptanceDigestAsync } from "@rightkit/legal/browser";
import type { LegalEligibilityBasis, LegalManifest } from "@rightkit/legal";
import { createNativeLegalReceiptPort, type LegalAcceptanceRecord } from "@rightkit/onboarding";
import { invoke } from "@tauri-apps/api/core";
import { create } from "zustand";
import rawManifest from "@rightkit-host/legal/legal-manifest.json";

export const legalManifest = rawManifest as unknown as LegalManifest;
export const REQUIRED_LEGAL_ACCEPTANCE_VERSION = legalManifest.acceptanceVersion;

export type { LegalAcceptanceRecord, LegalEligibilityBasis };

const receiptPort = createNativeLegalReceiptPort({ invoke });

export interface LegalStoreState {
  /** `loading` until the native receipt was read once; the gate renders nothing before then. */
  status: "loading" | "ready";
  /** The stored receipt for the current manifest, or `null` (none, or superseded). */
  receipt: LegalAcceptanceRecord | null;
  load: () => Promise<void>;
  accept: (basis: LegalEligibilityBasis) => Promise<void>;
}

export const useLegalStore = create<LegalStoreState>((set) => ({
  status: "loading",
  receipt: null,
  error: null,
  async load() {
    try {
      set({ status: "ready", receipt: (await receiptPort.read()) ?? null });
    } catch {
      // An unreadable receipt never satisfies the gate.
      set({ status: "ready", receipt: null });
    }
  },
  async accept(basis) {
    // A refusal throws: `LegalGate` shows it and the gate stays closed.
    const receipt = await receiptPort.create?.({
      version: legalManifest.acceptanceVersion,
      documentsHash: await materialAcceptanceDigestAsync(legalManifest),
      documents: acceptanceDeclaration(legalManifest).documents,
      eligibilityBasis: basis,
      surface: "first_run",
    });
    set({ receipt: receipt ?? null });
  },
}));

// The onboarding window and the retained main window both show the gate. When one accepts, the
// other re-reads the native receipt as it regains focus instead of keeping the gate it rendered
// before acceptance.
if (typeof window !== "undefined") {
  window.addEventListener("focus", () => {
    if (useLegalStore.getState().receipt === null) void useLegalStore.getState().load();
  });
}
