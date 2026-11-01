import { useEffect, useState } from "react";
import { isMac } from "../hooks/usePlatform";

// Staged reassurance under the hub loading spinner: silent for a normal
// (<5s) load; explains the rare Neural Engine model re-specialization
// (macOS cache eviction) that can hold the engine for ~30s-minutes.
export default function HubLoadingHint() {
  const [waitedSecs, setWaitedSecs] = useState(0);
  useEffect(() => {
    const id = window.setInterval(() => setWaitedSecs((s) => s + 1), 1000);
    return () => window.clearInterval(id);
  }, []);
  return (
    <span
      aria-live="polite"
      className="min-h-[16px] text-[length:var(--size-sm)] text-[var(--muted)]"
    >
      {waitedSecs >= 15
        ? isMac()
          ? "macOS is re-optimizing the speech model for the Neural Engine. This is rare and can take a few minutes. Dictation comes back online the moment it finishes."
          : "Optimizing the speech model… Dictation comes back when it finishes."
        : waitedSecs >= 5
          ? "Preparing the on-device speech model…"
          : ""}
    </span>
  );
}
