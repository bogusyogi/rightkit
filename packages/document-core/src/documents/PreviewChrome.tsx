import { useState, type ReactNode } from "react";
import { CaretLeft } from "@phosphor-icons/react/dist/icons/CaretLeft";
import { CaretRight } from "@phosphor-icons/react/dist/icons/CaretRight";
import { ArrowClockwise } from "@phosphor-icons/react/dist/icons/ArrowClockwise";
import { ViewportSwitch, type Viewport } from "./previewViewport";
import { ChromeButton } from "./ChromeButton";
import { DocumentToolbarSearch, type DocumentSearchMode } from "./DocumentToolbarFind";
import { writeClipboardText } from "../state/tauriBridge";

export type { Viewport };

function ChevronLeft() {
  return <CaretLeft size={14} weight="regular" aria-hidden />;
}
function ChevronRight() {
  return <CaretRight size={14} weight="regular" aria-hidden />;
}
function ReloadIcon() {
  return <ArrowClockwise size={14} weight="regular" aria-hidden />;
}

/** The trailing control that differs per surface: a disk file gets "source" (toggles the editable
 *  source drawer); a localhost URL preview gets "devtools" (opens the inspector). Everything else in
 *  the chrome is identical between the two. */
export interface ChromeTrailing {
  label: string;
  ariaLabel: string;
  active?: boolean;
  onClick: () => void;
}

/**
 * Shared preview chrome for BOTH the local-file HTML preview and the localhost URL preview. Identical
 * layout — copyable address bar with an inline reload, viewport switch, trailing control — so the two
 * surfaces match. The ONLY differences: the trailing button (`source` vs `devtools`), and Back/Forward
 * (rendered only when nav handlers are supplied; the URL preview can't drive a cross-origin iframe's
 * history, so it omits them).
 */
export function PreviewChrome({
  address,
  dirty = false,
  viewport,
  onViewport,
  onReload,
  canGoBack,
  canGoForward,
  onBack,
  onForward,
  trailing,
  findBar,
  findMode,
  onRequestFind,
  onRequestReplace,
  variant = "strip",
}: {
  /** "capsule": rendered inside the app's docked document bar (no strip border/background). */
  variant?: "strip" | "capsule";
  /** The path (file) or URL shown in the address bar; click to copy. */
  address: string;
  /** File only: active page has unsaved edits (shows a `*`). */
  dirty?: boolean;
  viewport: Viewport;
  onViewport: (v: Viewport) => void;
  onReload: () => void;
  /** Back/Forward render only when both handlers are provided (local-site nav). */
  canGoBack?: boolean;
  canGoForward?: boolean;
  onBack?: () => void;
  onForward?: () => void;
  /** Optional — Wave 2 Read mode on editable formats omits it entirely (the source drawer is an
   *  editing surface, so its toggle must not render). */
  trailing?: ChromeTrailing;
  findBar?: ReactNode;
  findMode?: DocumentSearchMode;
  onRequestFind?: () => void;
  onRequestReplace?: () => void;
}) {
  const hasNav = !!onBack && !!onForward;
  const navBack = onBack ?? (() => {});
  const navForward = onForward ?? (() => {});
  // Copy feedback shows INLINE at the address bar (a bottom toast is invisible over a white preview).
  const [copied, setCopied] = useState(false);
  const copyAddress = () => {
    void writeClipboardText(address)
      .then(() => {
        setCopied(true);
        window.setTimeout(() => setCopied(false), 1200);
      })
      .catch(() => {
        // Clipboard permission is optional; the inline confirmation simply stays hidden.
      });
  };
  return (
    <div
      className={
        variant === "capsule"
          ? "no-print vr-document-toolbar-search-layout relative flex min-w-0 items-center text-xs"
          : "no-print vr-chrome vr-document-toolbar vr-document-toolbar-search-layout relative min-w-0 shrink-0 border-b border-line bg-panel-2 text-xs"
      }
    >
      <div
        className={`vr-document-toolbar-main flex min-w-0 items-center gap-2 overflow-x-auto ${
          variant === "capsule" ? "px-1.5" : "py-1.5 pl-5 pr-3"
        }`}
      >
        {hasNav && (
          <>
            <ChromeButton ariaLabel="Back" disabled={!canGoBack} onClick={navBack}>
              <ChevronLeft />
            </ChromeButton>
            <ChromeButton ariaLabel="Forward" disabled={!canGoForward} onClick={navForward}>
              <ChevronRight />
            </ChromeButton>
          </>
        )}

        {/* Address bar: click to copy; reload pinned at the right edge, browser-style. Capped width so it
          doesn't crowd the trailing controls. */}
        <div className="hidden min-w-0 max-w-[54rem] flex-1 items-center rounded border border-line bg-ink/50 md:flex">
          <button
            type="button"
            onClick={copyAddress}
            title={`${address}\n(click to copy)`}
            className="vr-focus min-w-0 flex-1 truncate px-3 py-1 text-left font-mono text-muted hover:text-paper"
          >
            {address}
            {dirty && <span className="text-copper-hi"> *</span>}
          </button>
          {copied && (
            <span
              aria-live="polite"
              className="shrink-0 px-2 text-chrome-xs font-semibold text-emerald-400"
            >
              Copied
            </span>
          )}
          <button
            type="button"
            onClick={onReload}
            aria-label="Reload preview"
            title="Reload preview"
            className="vr-press vr-focus shrink-0 border-l border-line px-2 py-1 text-muted hover:text-copper-hi"
          >
            <ReloadIcon />
          </button>
        </div>

        {/* Narrow widths hide the address bar — keep reload reachable. */}
        <ChromeButton ariaLabel="Reload preview" onClick={onReload} className="md:hidden">
          <ReloadIcon />
        </ChromeButton>

        <div className="ml-auto flex shrink-0 items-center gap-2">
          <ViewportSwitch value={viewport} onChange={onViewport} />
          {trailing && (
            <ChromeButton
              ariaLabel={trailing.ariaLabel}
              active={trailing.active}
              onClick={trailing.onClick}
              className="px-2.5"
            >
              {trailing.label}
            </ChromeButton>
          )}
        </div>
      </div>
      <DocumentToolbarSearch
        findBar={findBar}
        activeMode={findMode}
        onRequestFind={onRequestFind}
        onRequestReplace={onRequestReplace}
      />
    </div>
  );
}
