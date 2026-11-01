// First run (M14, M12): the legal agreement gate, recorded by the native receipt, then the licence step when licensing is
// configured. `@rightkit/onboarding` owns the flow and its progress (settings key `onboarding.state`); the app supplies ports.
import { isLegalAccepted, Onboarding, type PersistedOnboardingState } from "@rightkit/onboarding";
import "@rightkit/legal-ui/legal-ui.css";
import "@rightkit/onboarding/onboarding.css";
import { legalGate, receiptDocuments, requiredDocumentsHash, requiredVersion } from "./lib/legal";
import { legalReceipt, licenseBackend, settings } from "./lib/services";
import { shell } from "./lib/shell";

/** True when the stored progress already covers every step this run would show. */
export function setupComplete(state: PersistedOnboardingState | undefined, licensing: boolean): boolean {
  if (!state || !isLegalAccepted(state, requiredVersion, undefined, requiredDocumentsHash)) return false;
  const done = [...state.completed, ...(state.skippedSteps ?? [])];
  return !licensing || done.includes("license");
}

export default function Setup({ licensing, onDone }: { licensing: boolean; onDone: () => void }) {
  return (
    <Onboarding
      settings={settings}
      legal={{ ...legalGate, onQuit: () => void shell?.quit({ force: true }) }}
      legalReceipt={legalReceipt}
      requiredVersion={requiredVersion}
      requiredDocumentsHash={requiredDocumentsHash}
      receiptDocuments={receiptDocuments}
      license={licensing ? licenseBackend : undefined}
      allowSkip={false}
      onComplete={onDone}
    />
  );
}
