import { useEffect, type ReactNode } from "react";
import { QA_BUILD } from "@rightkit/agent-ui-core/qaBuild";
import { useLegalStore } from "@rightkit/agent-ui-core/state/legal";
import LegalAcceptance from "./LegalAcceptance";

/**
 * Gates every the host app window (main app shell AND the onboarding window — see `main.tsx`) behind
 * the Right Suite Desktop License v1.1 first-run acceptance screen. The receipt is the native
 * legal receipt service's (read once through `useLegalStore`); the window renders nothing until
 * that read answers, so a gated window never flashes its content.
 *
 * Legal acceptance must happen before onboarding, since onboarding itself saves provider keys and
 * activates a license — both product operations the Privacy and Assent Standard requires to sit
 * behind the first-run gate.
 *
 * QA bypass mirrors the app's existing QA-gate convention (`qaBuild.ts`
 * `QA_BUILD`) so the hidden QA browser server (`right-qa.config.mjs`,
 * `VITE_RIGHTKIT_QA=1`) never needs a human to click through this screen.
 */
function qaBypassActive(): boolean {
  return QA_BUILD;
}

export function LegalGate({ children }: { children: ReactNode }) {
  const status = useLegalStore((state) => state.status);
  const receipt = useLegalStore((state) => state.receipt);
  const load = useLegalStore((state) => state.load);
  useEffect(() => {
    if (!qaBypassActive()) void load();
  }, [load]);
  if (qaBypassActive()) return <>{children}</>;
  if (status === "loading") return null;
  if (receipt === null) return <LegalAcceptance />;
  return <>{children}</>;
}

export default LegalGate;
