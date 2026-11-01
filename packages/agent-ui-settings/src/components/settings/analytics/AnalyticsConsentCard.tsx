import { useState } from "react";
import Icon from "@rightkit/agent-ui-core/components/Icon";
import type { AnalyticsProjection } from "@rightkit/agent-ui-core/lib/analytics/analyticsProjection";
import { readAnalyticsConsent, writeAnalyticsConsent } from "@rightkit/agent-ui-core/lib/analytics/analyticsProjection";
import { hostAppName } from "../../../appName";

export interface AnalyticsConsentCardProps {
  /** Optional native/background analytics handle. The UI remains usable when absent. */
  projection?: AnalyticsProjection;
  initialEnabled?: boolean;
  onChange?: (enabled: boolean) => void;
}

/**
 * Waku's anonymous-analytics setting, adapted to the host app settings tokens.
 * Consent is explicit state, can be withdrawn immediately, and never contains
 * a provider prompt, project path, response, or account identifier.
 */
export function AnalyticsConsentCard({
  projection,
  initialEnabled,
  onChange,
}: AnalyticsConsentCardProps) {
  const [enabled, setEnabled] = useState(
    () => initialEnabled ?? projection?.enabled ?? readAnalyticsConsent(),
  );

  const update = (next: boolean) => {
    setEnabled(next);
    projection?.setEnabled(next);
    if (!projection) writeAnalyticsConsent(next);
    onChange?.(next);
  };

  return (
    <section className="flex items-center justify-between gap-4 rounded-xl border border-line bg-panel px-4 py-3" aria-labelledby="analytics-consent-title">
      <div className="flex min-w-0 items-start gap-3">
        <span className="mt-0.5 text-text-3" aria-hidden="true"><Icon name="activity" size={16} /></span>
        <div className="min-w-0">
          <h4 id="analytics-consent-title" className="text-[13px] font-semibold text-text">Share anonymous usage data</h4>
          <p className="mt-1 text-[12px] leading-snug text-text-3">
            Help improve {hostAppName()} with coarse product events only. Prompts, project names and paths,
            provider output, and account identity are never included.
          </p>
        </div>
      </div>
      <label className="flex shrink-0 items-center gap-2 text-[12px] text-text-2">
        <input
          type="checkbox"
          className="h-6 w-6 cursor-pointer accent-accent"
          checked={enabled}
          onChange={(event) => update(event.currentTarget.checked)}
          aria-label="Share anonymous usage data"
        />
        {enabled ? "On" : "Off"}
      </label>
    </section>
  );
}

export default AnalyticsConsentCard;
