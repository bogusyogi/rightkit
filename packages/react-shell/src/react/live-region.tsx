import { useEffect, type ReactNode } from "react";
import { cx } from "./ui.js";

export type Politeness = "polite" | "assertive";

export interface LiveRegionProps {
  /** `polite` waits for a pause in speech (default); `assertive` interrupts. */
  politeness?: Politeness;
  /** The text announced when it changes. The node has no visible output of its own. */
  children?: ReactNode;
  className?: string;
}

/** A visually hidden `aria-live` node that announces its text when the text changes. */
export function LiveRegion({ politeness = "polite", children, className }: LiveRegionProps) {
  return (
    <div aria-live={politeness} aria-atomic="true" className={cx("rk-live-region", className)}>
      {children}
    </div>
  );
}

const regions: Partial<Record<Politeness, HTMLElement>> = {};

function regionFor(politeness: Politeness): HTMLElement {
  const existing = regions[politeness];
  if (existing?.isConnected) return existing;
  const el = document.createElement("div");
  el.className = "rk-live-region";
  el.setAttribute("aria-live", politeness);
  el.setAttribute("aria-atomic", "true");
  document.body.appendChild(el);
  regions[politeness] = el;
  return el;
}

/**
 * Announces `message` through a shared, visually hidden live region appended to the document once per politeness.
 * Use it for an announcement that comes from an event or an effect, when no `LiveRegion` is mounted for it. The text is
 * cleared and then set again, so a repeated message is announced again. An empty message announces nothing.
 */
export function useAnnounce(message: string | null | undefined, options: { politeness?: Politeness } = {}): void {
  const politeness = options.politeness ?? "polite";

  useEffect(() => {
    if (!message || typeof document === "undefined") return;
    const region = regionFor(politeness);
    region.textContent = "";
    const timer = setTimeout(() => {
      region.textContent = message;
    }, 50);
    return () => clearTimeout(timer);
  }, [message, politeness]);
}
