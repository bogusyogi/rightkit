// Wave 3 right-hand toolbar capsule: the "Aa" text-size popover (text formats) and the Info
// inspector toggle. Sits in the title bar's right pill area next to the per-format mode controls.
// The Aa popover is document-only: it steps the same per-surface zoom setting the Ctrl/Cmd+wheel
// handler in useDocumentZoom drives. App-wide look (theme, interface font) lives in the sidebar
// footer's Appearance menu and Settings → Theme — not here.
import { isMarkdownFormat } from "../../documents/capabilities";
import { memo, useCallback, useLayoutEffect, useRef, useState } from "react";
import { SidebarSimple } from "@phosphor-icons/react/dist/icons/SidebarSimple";
import { TextAa } from "@phosphor-icons/react/dist/icons/TextAa";
import type { LoadedDocument, ViewMode } from "@rightkit/document-core/documents/types";
import { BUNDLED_FONTS } from "@rightkit/document-core/settings/fonts";
import { useSettings } from "@rightkit/document-core/settings/settings";
import { useDismiss, useModalDismiss } from "@rightkit/platform-ui/headless";

// Formats whose reading/source text the shell's zoom settings actually size. HTML's interactive
// preview owns its own iframe zoom (Ctrl+wheel in-page), so its popover stepper follows the source
// drawer's zoom setting — see the comment in AppearancePopover.
const AA_FORMATS = new Set<LoadedDocument["format"]>(["md", "mdx", "txt", "html"]);

export const DocumentTools = memo(DocumentToolsImpl);

function DocumentToolsImpl({
  doc,
  view,
  inspectorOpen,
  onToggleInspector,
}: {
  doc: LoadedDocument;
  view: ViewMode;
  inspectorOpen: boolean;
  onToggleInspector: () => void;
}) {
  return (
    <div className="flex items-center gap-0.5">
      {AA_FORMATS.has(doc.format) ? <TextAppearanceButton doc={doc} view={view} /> : null}
      <button
        id="vr-inspector-toggle"
        type="button"
        aria-pressed={inspectorOpen}
        aria-controls="vr-info-inspector"
        aria-label={inspectorOpen ? "Hide document info" : "Show document info"}
        title={inspectorOpen ? "Hide info" : "Show info"}
        onClick={onToggleInspector}
        className={`vr-press vr-focus flex h-7 w-7 items-center justify-center rounded-full ${
          inspectorOpen
            ? "bg-copper/[0.14] text-copper-hi"
            : "text-muted-2 hover:bg-ink/40 hover:text-paper"
        }`}
      >
        {/* Mirrored sidebar glyph — the panel docks on the right. */}
        <SidebarSimple size={17} weight="regular" aria-hidden className="-scale-x-100" />
      </button>
    </div>
  );
}

const capsuleBtn = "vr-press vr-focus flex h-7 w-7 items-center justify-center rounded-full";

function TextAppearanceButton({ doc, view }: { doc: LoadedDocument; view: ViewMode }) {
  const [open, setOpen] = useState(false);
  const buttonRef = useRef<HTMLButtonElement>(null);
  // The popover takes focus on open (so Escape lands inside it); every close path returns focus
  // to the trigger button.
  const close = useCallback(() => {
    setOpen(false);
    buttonRef.current?.focus();
  }, []);
  return (
    <span className="relative">
      <button
        ref={buttonRef}
        type="button"
        aria-haspopup="dialog"
        aria-expanded={open}
        aria-controls={open ? "vr-aa-popover" : undefined}
        aria-label="Text size"
        title="Text size"
        onClick={() => (open ? close() : setOpen(true))}
        className={`${capsuleBtn} ${
          open ? "bg-copper/[0.14] text-copper-hi" : "text-muted-2 hover:bg-ink/40 hover:text-paper"
        }`}
      >
        <TextAa size={17} weight="regular" aria-hidden />
      </button>
      {open ? <AppearancePopover doc={doc} view={view} onClose={close} /> : null}
    </span>
  );
}

function AppearancePopover({
  doc,
  view,
  onClose,
}: {
  doc: LoadedDocument;
  view: ViewMode;
  onClose: () => void;
}) {
  const { settings, update } = useSettings();
  const panelRef = useRef<HTMLDivElement>(null);
  // Escape closes (window capture, stopPropagation only); a click on the scrim closes it.
  const { closing, close } = useModalDismiss(onClose, { escape: true, preventDefault: false });
  const scrimRef = useRef<HTMLDivElement>(null);
  useDismiss(() => close(), {
    escape: false,
    outside: "click-target",
    container: panelRef,
    backdrop: scrimRef,
    imeGuard: false,
  });

  // Focus the dialog on open so Escape is scoped to it and screen readers announce it.
  useLayoutEffect(() => {
    panelRef.current?.focus();
  }, []);

  // Same key/base/min/max the Ctrl/Cmd+wheel zoom in useDocumentZoom uses: reading surfaces take
  // fontSizePx (base 16, 12–28), source surfaces take sourceFontSizePx (base 12, 10–24). HTML's
  // preview zoom lives inside its opaque-origin iframe, so its shell-owned text size is the
  // source drawer's.
  const sourceSurface =
    doc.format === "html" || (isMarkdownFormat(doc.format) && view === "source");
  const zoomKey = sourceSurface ? ("sourceFontSizePx" as const) : ("fontSizePx" as const);
  const base = sourceSurface ? 12 : 16;
  const min = sourceSurface ? 10 : 12;
  const max = sourceSurface ? 24 : 28;
  const current = settings[zoomKey];
  const percent = Math.round((current / base) * 100);
  const step = (delta: number) =>
    update({ [zoomKey]: Math.max(min, Math.min(max, current + delta)) });

  return (
    <>
      <div ref={scrimRef} className="no-print fixed inset-0 z-[55]" aria-hidden />
      <div
        ref={panelRef}
        id="vr-aa-popover"
        role="dialog"
        aria-label="Text"
        tabIndex={-1}
        className={`vr-pop vr-pop-menu-tr${closing ? " vr-pop-exit" : ""} no-print vr-glass-panel absolute top-[calc(100%+8px)] right-0 z-[60] w-[220px] max-h-[calc(100vh-8rem)] overflow-y-auto rounded-[18px] p-3.5 outline-none`}
      >
        <h4 className="mb-1.5 font-sans text-chrome-xs font-semibold text-muted-2">Text size</h4>
        <div className="flex h-10 overflow-hidden rounded-xl bg-paper/5">
          <button
            type="button"
            aria-label="Decrease text size"
            title="Decrease text size"
            disabled={current <= min}
            onClick={() => step(-1)}
            className="vr-press vr-focus flex flex-1 items-center justify-center font-serif text-chrome-md text-paper hover:bg-paper/10 disabled:opacity-40"
          >
            A
          </button>
          <button
            type="button"
            aria-label="Increase text size"
            title="Increase text size"
            disabled={current >= max}
            onClick={() => step(1)}
            className="vr-press vr-focus flex flex-1 items-center justify-center border-l border-line font-serif text-[20px] leading-none text-paper hover:bg-paper/10 disabled:opacity-40"
          >
            A
          </button>
        </div>
        <div className="mt-1.5 flex items-center justify-between px-0.5">
          <span className="font-mono text-chrome-xs tabular-nums text-muted-2">{percent}%</span>
          <button
            type="button"
            disabled={current === base}
            onClick={() => update({ [zoomKey]: base })}
            title="Reset text size"
            className="vr-press vr-focus rounded-md px-2 py-1 font-sans text-chrome-xs text-muted hover:bg-paper/5 hover:text-paper disabled:opacity-40"
          >
            Reset
          </button>
        </div>
        {!sourceSurface && doc.format === "md" && (
          <>
            <h4
              id="vr-aa-width-label"
              className="mb-1.5 mt-3 font-sans text-chrome-xs font-semibold text-muted-2"
            >
              Content width
            </h4>
            <div role="group" aria-labelledby="vr-aa-width-label">
              <div className="flex h-9 overflow-hidden rounded-xl bg-paper/5">
                {(["full", "column"] as const).map((mode) => (
                  <button
                    key={mode}
                    type="button"
                    aria-pressed={settings.readingWidthMode === mode}
                    onClick={() => update({ readingWidthMode: mode })}
                    className={`vr-press vr-focus flex-1 whitespace-nowrap px-2 font-sans text-chrome-xs ${
                      settings.readingWidthMode === mode
                        ? "bg-copper/[0.14] text-copper-hi"
                        : "text-muted hover:bg-paper/10 hover:text-paper"
                    }`}
                  >
                    {mode === "full" ? "Full width" : "Reading column"}
                  </button>
                ))}
              </div>
              {settings.readingWidthMode === "column" && (
                <div className="mt-2">
                  <input
                    type="range"
                    aria-label="Reading column width"
                    min={560}
                    max={960}
                    step={20}
                    value={settings.readingWidthPx}
                    onChange={(e) => update({ readingWidthPx: Number(e.target.value) })}
                    className="w-full accent-[var(--copper)]"
                  />
                  <div className="mt-1 flex items-center justify-between px-0.5">
                    <span className="font-mono text-chrome-xs tabular-nums text-muted-2">
                      {settings.readingWidthPx}px
                    </span>
                    <button
                      type="button"
                      disabled={settings.readingWidthPx === 700}
                      onClick={() => update({ readingWidthPx: 700 })}
                      title="Reset column width"
                      className="vr-press vr-focus rounded-md px-2 py-1 font-sans text-chrome-xs text-muted hover:bg-paper/5 hover:text-paper disabled:opacity-40"
                    >
                      Reset
                    </button>
                  </div>
                </div>
              )}
            </div>
          </>
        )}
        {!sourceSurface && (
          <>
            <h4
              id="vr-aa-font-label"
              className="mb-1.5 mt-3 font-sans text-chrome-xs font-semibold text-muted-2"
            >
              Reading font
            </h4>
            <div
              role="radiogroup"
              aria-labelledby="vr-aa-font-label"
              className="flex flex-col gap-0.5"
            >
              {READING_FONTS.map((f) => {
                const checked = settings.fontFamily === f.id;
                return (
                  <button
                    key={f.id}
                    type="button"
                    role="radio"
                    aria-checked={checked}
                    onClick={() => update({ fontFamily: f.id })}
                    className={`vr-press vr-focus flex h-8 items-center justify-between rounded-lg px-2.5 text-left text-[14px] text-paper hover:bg-paper/10${checked ? " bg-paper/10" : ""}`}
                  >
                    <span style={{ fontFamily: f.stack }}>{f.name}</span>
                    {checked && (
                      <span aria-hidden="true" className="text-chrome-sm text-muted">
                        ✓
                      </span>
                    )}
                  </button>
                );
              })}
            </div>
          </>
        )}
      </div>
    </>
  );
}

/** Reading fonts offered for Markdown and text (monospace faces stay for code and source). */
const READING_FONTS = BUNDLED_FONTS.filter((f) => f.kind !== "mono");
