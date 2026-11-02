import { useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { onboardingStateKey, type PersistedOnboardingState } from "@rightkit/onboarding";
import { legalReceipt } from "../lib/legal.js";
import { onboardingSettings } from "../lib/onboardingSettings.js";
import { FLOW_KEY, freshFlow, writeSetting } from "./flow.js";

/** Settings > General > "Show onboarding again" fires this on `window` (Settings.tsx). */
export const SHOW_ONBOARDING = "pulse-show-onboarding";

async function savedState(): Promise<Partial<PersistedOnboardingState> | undefined> {
  const snapshot = await onboardingSettings.load();
  return snapshot.values[onboardingStateKey()] as Partial<PersistedOnboardingState> | undefined;
}

/**
 * Whether the native receipt holds the current agreement (version and document hash). The first read also carries over
 * one the web view recorded earlier, including one saved under the pre-port digest of the same documents (#475).
 */
async function legalAccepted(): Promise<boolean> {
  return (await legalReceipt.read()) !== null;
}

/**
 * Re-run: puts the shared record back to its first app step (the agreement, once accepted, is kept), starts a flow that
 * keeps the current values, marks setup unfinished in the notch so an interrupted re-run resumes, and opens the window.
 */
async function rerun(section: string) {
  const saved = await savedState();
  await onboardingSettings.patch({
    ...(saved ? { [onboardingStateKey()]: { ...saved, currentStep: 1, completed: [], skipped: false, skippedSteps: [] } } : {}),
    [FLOW_KEY]: freshFlow(false),
  });
  // Unconfirmed, the re-run still opens: the window itself decides again when it finishes.
  await writeSetting("onboardingDone", false);
  await invoke("onboarding_open", { section });
}

/**
 * The hub window's part of first run. It never renders onboarding: the first-run window is its own window
 * (`onboarding.rs`). The native side opens it instead of the hub while the notch says setup is unfinished; this hook
 * covers the two cases the native side cannot see, a new legal agreement and "Show onboarding again".
 */
export function useFirstRunGate(section: string) {
  useEffect(() => {
    void legalAccepted()
      .then((ok) => (ok ? undefined : invoke("onboarding_open", { section })))
      .catch(() => {});
    // Once, when the hub window opens.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  useEffect(() => {
    const again = () => void rerun(section).catch(() => {});
    window.addEventListener(SHOW_ONBOARDING, again);
    return () => window.removeEventListener(SHOW_ONBOARDING, again);
  }, [section]);
}
