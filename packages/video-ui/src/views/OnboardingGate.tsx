import { Onboarding, isLegalAccepted, onboardingStateKey, type PersistedOnboardingState } from "@rightkit/onboarding";
import "@rightkit/onboarding/onboarding.css";
import { type ReactNode, useEffect, useMemo, useState } from "react";
import { LEGAL_MATERIAL_SHA256, REQUIRED_LEGAL_ACCEPTANCE_VERSION, legalStepConfig } from "../legal/legalConfig";
import { createModelsBackend } from "../lib/modelsBackend";
import { createHostSettingsBackend } from "../lib/settingsBackend";

type Phase = "loading" | "setup" | "done";

/**
 * First-run gate: @rightkit/onboarding owns the legal agreement (documents expand inline
 * in the shared gate) and the model step. The workspace mounts only after the current legal
 * documents are accepted and setup is completed or skipped; a changed document re-prompts.
 * A broken settings store never wedges the app.
 */
export function OnboardingGate({ children }: { children: ReactNode }) {
  const [phase, setPhase] = useState<Phase>("loading");
  const settings = useMemo(() => createHostSettingsBackend(), []);
  const models = useMemo(() => createModelsBackend(), []);
  const legal = useMemo(() => legalStepConfig(), []);

  useEffect(() => {
    let live = true;
    // One failed read must not send a set-up user back through first-run setup, so the
    // read is tried again before the gate gives up and shows setup.
    void (async () => {
      for (let attempt = 0; ; attempt += 1) {
        try {
          const snap = await settings.load();
          if (!live) return;
          const st = snap.values[onboardingStateKey()] as PersistedOnboardingState | undefined;
          const done = st && isLegalAccepted(st, REQUIRED_LEGAL_ACCEPTANCE_VERSION, undefined, LEGAL_MATERIAL_SHA256)
            && (st.skipped === true || (st.completed ?? []).includes("models") || (st.skippedSteps ?? []).includes("models"));
          setPhase(done ? "done" : "setup");
          return;
        } catch {
          if (!live) return;
          if (attempt === 2) {
            setPhase("setup");
            return;
          }
          await new Promise((next) => setTimeout(next, 1000));
        }
      }
    })();
    return () => {
      live = false;
    };
  }, [settings]);

  if (phase === "loading") return null;
  if (phase === "done") return <>{children}</>;
  return (
    <Onboarding
      settings={settings}
      legal={legal}
      models={models}
      requiredVersion={REQUIRED_LEGAL_ACCEPTANCE_VERSION}
      requiredDocumentsHash={LEGAL_MATERIAL_SHA256}
      documentsHash={LEGAL_MATERIAL_SHA256}
      onComplete={() => setPhase("done")}
      onSkip={() => setPhase("done")}
    />
  );
}
