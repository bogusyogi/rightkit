import { useEffect, useState } from "react";
import { cx } from "../lib/ui.js";
import { onboardingClient } from "./client.js";
import { METER_BARS } from "./constants.js";

export function MicMeter({ active }: { active: boolean }) {
  // Owns its own event subscription so the ~12.5Hz mic-level stream re-renders
  // this leaf meter instead of the whole wizard tree.
  const [level, setLevel] = useState(0);
  useEffect(() => {
    if (!active) {
      setLevel(0);
      return;
    }
    const unlisten = onboardingClient().on("micLevel", (payload) => {
      setLevel(Math.max(0, Math.min(1, Number(payload.level) || 0)));
    });
    return () => {
      void unlisten.then((fn) => fn()).catch(() => undefined);
    };
  }, [active]);
  const clamped = Math.max(0, Math.min(1, level));
  const activeCount = clamped <= 0 ? 0 : Math.max(1, Math.round(clamped * 12));
  return (
    <>
      <meter
        className="sr-only"
        min={0}
        max={1}
        value={clamped}
        aria-label="Microphone signal meter"
      >
        {Math.round(clamped * 100)}%
      </meter>
      <div
        className="grid h-8 grid-cols-12 items-center gap-1.5 rounded-full bg-[var(--surface-1)] px-3"
        aria-hidden="true"
      >
        {METER_BARS.map((bar) => {
          const i = bar - 1;
          return (
            <span
              key={bar}
              className={cx(
                "h-10 rounded-full transition-[height,background-color,opacity] duration-100",
                i < activeCount ? "bg-[var(--accent)] opacity-100" : "bg-[var(--surface-3)] opacity-80",
              )}
              style={{ height: `${9 + Math.sin((i + 1) * 0.9) * 4 + clamped * 17}px` }}
            />
          );
        })}
      </div>
    </>
  );
}

export function IdlePillPreview() {
  return (
    <span
      className="inline-block h-[10px] w-[52px] rounded-full bg-[var(--paper)] shadow-[var(--surface-shadow)]"
      aria-hidden="true"
    />
  );
}
