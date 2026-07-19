import { useId, useState, type CSSProperties, type ReactElement } from "react";

export type LegalGateVariant = "first-run" | "re-acceptance";
export type LegalGateEligibilityBasis = "individual" | "enterprise";

/**
 * Light-only theme (locked by Adrian 2026-07-17): the legal gate always
 * renders on the app's LIGHT base tokens from brands.md, regardless of the
 * app's own theme setting. There is no mode switch.
 */
export interface LegalGateTheme {
  bg: string;
  surface: string;
  border: string;
  text: string;
  muted: string;
  accent: string;
  accentContrast: string;
  /** Wordmark face. LOCKED to Tanker suite-wide — hosts must ship Tanker
   * (or pass `wordmarkFontUrl` and keep "Tanker" first in this stack). */
  fontDisplay: string;
  fontBody: string;
}

export interface LegalGateDocument {
  title: string;
  url: string;
}

export interface LegalGateProps {
  theme: LegalGateTheme;
  appName: string;
  trustLine: string;
  documents: LegalGateDocument[];
  agreementStatement: string;
  variant: LegalGateVariant;
  onAgree: (input: { eligibilityBasis: LegalGateEligibilityBasis }) => void | Promise<void>;
  onQuit: () => void;
  dbaLine?: string;
  /** Overrides the built-in Orthic Labs mark with an image URL. The mark is
   * inlined by default (see OrthicMark) so no asset wiring is needed; pass this
   * only if a host must ship its own file. Note the on-disk
   * rightsuite/brand-assets marks are dark-background variants and will not
   * read on this gate's light base. */
  parentLogoSrc?: string;
  /** Optional URL of the Tanker woff2. When provided, the component registers
   * an @font-face for family "Tanker" (weight 400) so the wordmarks render in
   * the locked suite face even if the host app hasn't loaded Tanker itself.
   * theme.fontDisplay must still list "Tanker" first for this to take effect. */
  wordmarkFontUrl?: string;
}

const DEFAULT_DBA_LINE = "Orthic Labs is a trading name of Damned Ventures LLC.";
/** Orthic Labs' own accent (tokens.json). The parent mark keeps it on every
 * app's gate — it is the publisher's color, not the product's. */
const ORTHIC_EMBER = "#FF5630";

/** The Orthic Labs mark: geometry copied verbatim from the canonical asset
 * (rightsuite/brand-assets/orthic-labs-mark.svg). That file strokes the frame in
 * cream (#F2EFE6) because it is the dark-background variant — on the gate's cream
 * base it disappears and leaves a floating ember corner. So the frame takes the
 * host's ink color while the corner stays Orthic's ember, which is the parent
 * brand's own accent and does not follow the app. Inlined rather than loaded:
 * the component must stay self-contained across five different app bundlers. */
function OrthicMark({ frameColor, size = 26 }: { frameColor: string; size?: number }): ReactElement {
  return (
    <svg
      viewBox="0 0 120 120"
      width={size}
      height={size}
      aria-hidden="true"
      focusable="false"
      style={{ display: "block", flex: "none" }}
    >
      <path
        d="M10 64V10H110V110H56"
        fill="none"
        stroke={frameColor}
        strokeWidth={24}
        strokeLinecap="butt"
        strokeLinejoin="miter"
      />
      <path
        d="M10 68V110H52"
        fill="none"
        stroke={ORTHIC_EMBER}
        strokeWidth={24}
        strokeLinecap="butt"
        strokeLinejoin="miter"
      />
    </svg>
  );
}

/** Splits a one-word CamelCase Right Suite wordmark ("HeardRight") into its
 * base and the trailing "Right" that carries the app's accent color. Falls
 * back to treating the whole name as the base if it doesn't end in "Right". */
function splitWordmark(appName: string): { base: string; suffix: string } {
  if (appName.endsWith("Right") && appName.length > "Right".length) {
    return { base: appName.slice(0, -"Right".length), suffix: "Right" };
  }
  return { base: appName, suffix: "" };
}

let styleInjected = false;
function ensureStyle(): void {
  if (styleInjected || typeof document === "undefined") return;
  styleInjected = true;
  const style = document.createElement("style");
  style.setAttribute("data-rightkit-legal-gate", "");
  style.textContent = `
.rk-legal-gate a { color: inherit; text-decoration: underline; text-decoration-color: currentColor; text-underline-offset: 3px; opacity: 0.72; transition: opacity 120ms ease, color 120ms ease; }
.rk-legal-gate a:hover { opacity: 1; }
.rk-legal-gate button { font: inherit; cursor: pointer; }
.rk-legal-gate button:disabled { cursor: not-allowed; }
.rk-legal-gate input[type="radio"],
.rk-legal-gate input[type="checkbox"] { accent-color: var(--rk-accent); width: 18px; height: 18px; flex: none; margin: 2px 0 0; }
.rk-legal-gate a:focus-visible,
.rk-legal-gate button:focus-visible,
.rk-legal-gate input:focus-visible { outline: 2px solid var(--rk-accent); outline-offset: 3px; border-radius: 3px; }
.rk-legal-gate__radio-row,
.rk-legal-gate__checkbox-row { display: flex; align-items: flex-start; gap: 12px; cursor: pointer; }
.rk-legal-gate__agree:not(:disabled):hover { filter: brightness(1.06); }
.rk-legal-gate__quit:hover { opacity: 1; text-decoration: underline; }
.rk-legal-gate .rk-sr-only { position: absolute; width: 1px; height: 1px; padding: 0; margin: -1px; overflow: hidden; clip: rect(0, 0, 0, 0); white-space: nowrap; border: 0; }
/* Two landscape rails: every Right Suite window is wider than it is tall, and
   HeardRight's is only 660x400 — a single stacked column does not fit and would
   scroll a contract. Left rail says who is asking; right rail is the decision. */
.rk-legal-gate__body { display: grid; grid-template-columns: minmax(0, 1fr) minmax(0, 1fr); column-gap: 48px; align-items: start; width: 100%; max-width: 780px; flex: none; }
.rk-legal-gate__rail { min-width: 0; }
.rk-legal-gate__dba { grid-column: 1 / -1; }
/* Below the two-rail threshold (narrow or resized-small windows) it stacks. */
@media (max-width: 640px) {
  .rk-legal-gate__body { grid-template-columns: minmax(0, 1fr); row-gap: 26px; max-width: 420px; }
}
`;
  document.head.appendChild(style);
}

const injectedWordmarkFonts = new Set<string>();
function ensureWordmarkFont(url: string): void {
  if (typeof document === "undefined" || injectedWordmarkFonts.has(url)) return;
  injectedWordmarkFonts.add(url);
  const style = document.createElement("style");
  style.setAttribute("data-rightkit-legal-gate-font", url);
  style.textContent = `@font-face { font-family: "Tanker"; src: url("${url}") format("woff2"); font-weight: 400; font-style: normal; font-display: swap; }`;
  document.head.appendChild(style);
}

export function LegalGate({
  theme,
  appName,
  trustLine,
  documents,
  agreementStatement,
  variant,
  onAgree,
  onQuit,
  dbaLine = DEFAULT_DBA_LINE,
  parentLogoSrc,
  wordmarkFontUrl,
}: LegalGateProps): ReactElement {
  ensureStyle();
  if (wordmarkFontUrl) ensureWordmarkFont(wordmarkFontUrl);

  const [eligibilityBasis, setEligibilityBasis] = useState<LegalGateEligibilityBasis | null>(null);
  const [agreed, setAgreed] = useState(false);
  const [submitting, setSubmitting] = useState(false);

  const headlineId = useId();
  const trustLineId = useId();
  const individualId = useId();
  const enterpriseId = useId();
  const agreeCheckboxId = useId();

  const { base, suffix } = splitWordmark(appName);
  const canAgree = eligibilityBasis !== null && agreed && !submitting;

  const handleAgree = (): void => {
    if (eligibilityBasis === null || !agreed || submitting) return;
    const result = onAgree({ eligibilityBasis });
    if (result && typeof (result as Promise<void>).then === "function") {
      setSubmitting(true);
      void (result as Promise<void>).finally(() => setSubmitting(false));
    }
  };

  const rootStyle: CSSProperties = {
    ["--rk-accent" as string]: theme.accent,
    background: theme.bg,
    color: theme.text,
    fontFamily: theme.fontBody,
    minHeight: "100vh",
    width: "100%",
    display: "flex",
    flexDirection: "column",
    alignItems: "center",
    boxSizing: "border-box",
    padding: "28px 36px 24px",
  };

  return (
    <div className="rk-legal-gate" style={rootStyle}>
      {/* The lockup band is a flex spacer with the lockup centered inside it, and
          an equal spacer sits below the content — so the lockup lands midway
          between the top edge and the app wordmark at any window height, and the
          content stays optically centered. Adrian's placement, 2026-07-17. */}
      <div style={{ flex: "1 0 auto", display: "flex", alignItems: "center", minHeight: 40 }}>
        {/* The publisher lockup is page chrome, not form content, and is the ONLY
            centered element — the app wordmark below is left-aligned. */}
        <div
          className="rk-legal-gate__lockup"
          style={{ display: "flex", alignItems: "center", justifyContent: "center", gap: 10 }}
        >
          {parentLogoSrc ? (
            <img src={parentLogoSrc} alt="" aria-hidden="true" style={{ height: 24, width: "auto" }} />
          ) : (
            <OrthicMark frameColor={theme.text} size={24} />
          )}
          {/* Matches the mark's box height so the two read as one lockup. */}
          <div
            style={{
              fontFamily: theme.fontDisplay,
              fontSize: 24,
              letterSpacing: "0.06em",
              textTransform: "uppercase",
              color: theme.text,
              lineHeight: 1,
            }}
          >
            Orthic Labs
          </div>
        </div>
      </div>

      <form
        className="rk-legal-gate__body"
        aria-labelledby={headlineId}
        aria-describedby={trustLineId}
        onSubmit={(event) => {
          event.preventDefault();
          handleAgree();
        }}
      >
        {/* Left rail: who is asking, and what they can read before agreeing. */}
        <div className="rk-legal-gate__rail">
          <header className="rk-legal-gate__wordmark" style={{ marginBottom: 14 }}>
            <h1
              id={headlineId}
              style={{
                fontFamily: theme.fontDisplay,
                fontWeight: 400,
                fontSize: variant === "first-run" ? 34 : 26,
                lineHeight: 1.06,
                letterSpacing: "-0.02em",
                margin: 0,
                textAlign: "left",
              }}
            >
              {base}
              <span style={{ color: theme.accent }}>{suffix}</span>
            </h1>
          </header>

          {variant === "re-acceptance" ? (
            <p
              style={{
                fontFamily: theme.fontBody,
                fontSize: 14,
                lineHeight: 1.5,
                color: theme.text,
                margin: 0,
              }}
            >
              The terms below have changed. Agree to continue, or quit.
            </p>
          ) : null}

          <p
            id={trustLineId}
            style={{
              fontFamily: theme.fontBody,
              fontSize: 14,
              lineHeight: 1.55,
              color: theme.muted,
              margin: variant === "first-run" ? "0 0 22px" : "8px 0 22px",
            }}
          >
            {trustLine}
          </p>

          <nav aria-label={`${appName} legal documents`}>
            <ul style={{ listStyle: "none", margin: 0, padding: 0, display: "flex", flexDirection: "column", gap: 6 }}>
              {documents.map((document) => (
                <li key={document.url}>
                  <a href={document.url} target="_blank" rel="noreferrer" style={{ fontSize: 13.5 }}>
                    {document.title}
                  </a>
                </li>
              ))}
            </ul>
          </nav>
        </div>

        {/* Right rail: the decision itself. */}
        <div className="rk-legal-gate__rail">
        <fieldset style={{ border: "none", padding: 0, margin: "0 0 16px" }}>
          <legend className="rk-sr-only">Who is this license for</legend>
          <div style={{ display: "flex", flexDirection: "column", gap: 11 }}>
            <label htmlFor={individualId} className="rk-legal-gate__radio-row">
              <input
                id={individualId}
                type="radio"
                name="rk-legal-gate-eligibility"
                value="individual"
                checked={eligibilityBasis === "individual"}
                onChange={() => setEligibilityBasis("individual")}
              />
              <span style={{ fontSize: 14, lineHeight: 1.4 }}>
                For myself
                <br />
                <span style={{ color: theme.muted, fontSize: 12.5 }}>
                  One person, my own devices, including my own freelance work.
                </span>
              </span>
            </label>
            <label htmlFor={enterpriseId} className="rk-legal-gate__radio-row">
              <input
                id={enterpriseId}
                type="radio"
                name="rk-legal-gate-eligibility"
                value="enterprise"
                checked={eligibilityBasis === "enterprise"}
                onChange={() => setEligibilityBasis("enterprise")}
              />
              <span style={{ fontSize: 14, lineHeight: 1.4 }}>
                For an organization
                <br />
                <span style={{ color: theme.muted, fontSize: 12.5 }}>
                  Seats are licensed under an Enterprise Order.
                </span>
              </span>
            </label>
          </div>
        </fieldset>

        <label htmlFor={agreeCheckboxId} className="rk-legal-gate__checkbox-row" style={{ marginBottom: 24 }}>
          <input
            id={agreeCheckboxId}
            type="checkbox"
            checked={agreed}
            onChange={(event) => setAgreed(event.target.checked)}
          />
          <span style={{ fontSize: 13.5, lineHeight: 1.5, color: theme.text }}>{agreementStatement}</span>
        </label>

        <div style={{ display: "flex", alignItems: "center", gap: 20 }}>
          <button
            type="submit"
            className="rk-legal-gate__agree"
            disabled={!canAgree}
            style={{
              background: theme.accent,
              color: theme.accentContrast,
              border: "none",
              borderRadius: 6,
              padding: "11px 22px",
              fontSize: 14,
              fontWeight: 600,
              opacity: canAgree ? 1 : 0.45,
            }}
          >
            Agree &amp; continue
          </button>
          <button
            type="button"
            className="rk-legal-gate__quit"
            onClick={onQuit}
            style={{
              background: "transparent",
              border: "none",
              color: theme.muted,
              fontSize: 14,
              padding: "11px 4px",
              opacity: 0.85,
            }}
          >
            Quit
          </button>
        </div>

        <p
          className="rk-legal-gate__dba"
          style={{
            fontFamily: theme.fontBody,
            fontSize: 12,
            lineHeight: 1.6,
            color: theme.muted,
            margin: "20px 0 0",
            opacity: 0.85,
          }}
        >
          {dbaLine}
        </p>
        </div>
      </form>

      {/* Mirrors the lockup band so the content sits optically centered. */}
      <div style={{ flex: "1 0 auto", minHeight: 16 }} />
    </div>
  );
}
