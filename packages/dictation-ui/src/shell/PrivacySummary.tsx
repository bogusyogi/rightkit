import { memo } from "react";
import { CompactSettingsGroup } from "../components/settings/CompactSettingsGroup.js";
import { hostAppName } from "../host.js";
import { AI_POLISH_PRIVACY_NOTICE } from "../lib/privacyDisclosure.js";

// Privacy & data — a read-only group (inset card, label above) holding the
// one-paragraph data story: what AI polish sends, and that diagnostics stay
// redacted. Deliberately control-free: diagnostics opt-out lives in
// Developer Mode, and feedback consent is the explicit send on this page.
function PrivacySummary() {
  return (
    <CompactSettingsGroup title="Privacy &amp; data">
      <div className="px-[var(--space-4)] py-[var(--space-3)]">
        <p className="m-0 max-w-[640px] text-[length:var(--size-sm)] leading-[1.55] text-[var(--muted)]">
          {AI_POLISH_PRIVACY_NOTICE} Feedback can attach redacted diagnostics. {hostAppName()} sends no
          automatic telemetry.
        </p>
      </div>
    </CompactSettingsGroup>
  );
}

export default memo(PrivacySummary);
