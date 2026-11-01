import type { ReactNode } from "react";

/** Browser-style preview viewport: full-width desktop vs. constrained tablet/phone device frames.
 *  Shared by the HTML document chrome (HtmlDocumentChrome) and the Live-Preview pane so both panes
 *  offer the same desktop/tablet/phone responsive preview. */
export type Viewport = "desktop" | "tablet" | "mobile";

/** Constrained device-frame width in px, or null for full-width desktop. */
export function viewportWidthPx(viewport: Viewport): number | null {
  return viewport === "mobile" ? 390 : viewport === "tablet" ? 768 : null;
}

/** Map an available container width to the viewport it best fits — used to auto-pick a frame when
 *  "desktop" is selected but the pane itself is narrow. */
export function viewportForPreviewWidth(width: number): Viewport {
  if (width <= 520) return "mobile";
  if (width <= 820) return "tablet";
  return "desktop";
}

/** Desktop / tablet / phone toggle for a preview pane. */
export function ViewportSwitch({
  value,
  onChange,
}: {
  value: Viewport;
  onChange: (v: Viewport) => void;
}) {
  return (
    <div className="flex shrink-0 items-center gap-2" role="group" aria-label="Preview viewport">
      <ViewportButton
        label="Desktop"
        active={value === "desktop"}
        onClick={() => onChange("desktop")}
      >
        <svg width="13" height="13" viewBox="0 0 16 16" aria-hidden>
          <rect x="1.5" y="2.5" width="13" height="9" rx="1" fill="none" stroke="currentColor" />
          <path d="M6 14h4M8 11.5V14" stroke="currentColor" />
        </svg>
      </ViewportButton>
      <ViewportButton label="Tablet" active={value === "tablet"} onClick={() => onChange("tablet")}>
        <svg width="13" height="13" viewBox="0 0 16 16" aria-hidden>
          <rect x="3.5" y="1.5" width="9" height="13" rx="1.2" fill="none" stroke="currentColor" />
          <path d="M7 12.5h2" stroke="currentColor" />
        </svg>
      </ViewportButton>
      <ViewportButton label="Mobile" active={value === "mobile"} onClick={() => onChange("mobile")}>
        <svg width="13" height="13" viewBox="0 0 16 16" aria-hidden>
          <rect x="5" y="1.5" width="6" height="13" rx="1.2" fill="none" stroke="currentColor" />
          <path d="M7.5 12.5h1" stroke="currentColor" />
        </svg>
      </ViewportButton>
    </div>
  );
}

function ViewportButton({
  label,
  active,
  onClick,
  children,
}: {
  label: string;
  active: boolean;
  onClick: () => void;
  children: ReactNode;
}) {
  return (
    <button
      type="button"
      aria-label={label}
      aria-pressed={active}
      title={label}
      onClick={onClick}
      className={`vr-press vr-focus flex items-center rounded border px-1.5 py-1 ${
        active
          ? "border-copper/60 bg-copper/20 text-copper-hi"
          : "border-line text-muted hover:border-copper hover:text-copper-hi"
      }`}
    >
      {children}
    </button>
  );
}
