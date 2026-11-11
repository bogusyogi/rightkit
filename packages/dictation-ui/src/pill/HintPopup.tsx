import { useCallback, useEffect, useRef, useState } from "react";
import { X } from "@phosphor-icons/react";
import { dictationStorageKey, hostAppName } from "../host.js";
import { listenPill, pillClient } from "./pillHost.js";
import { CSS as PILL_CSS } from "./pillStyles.js";
import { pillTooltipProps } from "./pillTooltip.js";
import { getPlatform } from "@rightkit/platform-ui";
// Single source of truth for hint TEXT, shared with the macOS Swift pill (which
// reads the same JSON). Add a hint there, not here — this renderer is just the
// Windows container. See src/shared/hint-catalog.json.
import hintCatalog from "../shared/hint-catalog.json" with { type: "json" };

type ScreenshotDestination = "clipboard" | "disk";
type FeatureHint = { title: string; body: string };
// Feature hints (plain title/body) come entirely from the shared catalog; the
// `_`-prefixed doc key is not a hint. `kind` is validated at runtime against
// this map (`payload.kind in FEATURE_HINTS`), so the type stays a plain string.
type FeatureHintKind = string;
// Per-platform hint text: a plain string is identical on both OSes; an object
// carries the OS-specific variant (keybinds differ — Ctrl+Shift+Space vs fn).
type LocalizedText = string | { windows: string; macos: string };
type CatalogEntry = { platform?: "both" | "windows" | "macos"; title: LocalizedText; body: LocalizedText };
// This is the Windows WebView pill, so resolve to the Windows variant (the Swift
// pill resolves to macOS from the same file). Non-mac hosts fall back to Windows.
const HINT_PLATFORM: "windows" | "macos" = getPlatform() === "macos" ? "macos" : "windows";
function resolveHintText(text: LocalizedText): string {
  return typeof text === "string" ? text : text[HINT_PLATFORM];
}
// Catalog copy names the product as `{app}`; the host's name fills it in at render time.
function withAppName(text: string): string {
  return text.replaceAll("{app}", hostAppName());
}
const FEATURE_HINTS: Record<string, FeatureHint> = (() => {
  const out: Record<string, FeatureHint> = {};
  for (const [key, value] of Object.entries(hintCatalog)) {
    if (key.startsWith("_") || typeof value !== "object" || value === null) continue;
    const entry = value as CatalogEntry;
    // Skip hints scoped to the other OS (e.g. apple_shortcuts is macOS-only).
    if (entry.platform && entry.platform !== "both" && entry.platform !== HINT_PLATFORM) continue;
    out[key] = { title: resolveHintText(entry.title), body: resolveHintText(entry.body) };
  }
  return out;
})();
type HintView = { kind: "hidden" } | { kind: "screenshot_prompt" } | { kind: "prompt_hint" } | { kind: FeatureHintKind };
type PillHintPayload = {
  kind?: string;
};

const HINT_CSS = `
html,body,#root{margin:0;width:100%;height:100%;background:transparent;overflow:hidden}
.hr-pill-hint-root{
  box-sizing:border-box; width:100vw; height:100vh;
  display:flex; align-items:flex-end; justify-content:center;
  font-family:'Hanken Grotesk',system-ui,-apple-system,sans-serif;
  color:rgba(247,244,239,.94); -webkit-user-select:none; user-select:none;
}
/* flex:none — the window is sized to hug this card, so letting the flex parent
   shrink it would make its width a function of the window it drives (the clip's
   2026-07-26 collapse loop). */
.hr-pill-hint-card{display:inline-flex; flex:none}
.hr-hint-strip{
  box-sizing:border-box; min-height:34px; max-width:420px; padding:4px 5px 4px 12px;
  display:inline-flex; align-items:center; gap:6px; white-space:nowrap;
  overflow:hidden;
  border-radius:999px; color:#f5f2ee; background:#000;
  border:1px solid rgba(51,51,51,.72);
  /* Swift CenteredAuxiliaryEntrance: 0.965 -> 1 with PillMotion.settled. */
  animation:hrHintStripIn 150ms var(--hr-aux-settled-spring) both;
}
.hr-hint-strip strong,.hr-hint-strip span{font-size:12px; line-height:1; letter-spacing:0}
.hr-hint-strip strong{font-weight:780}
.hr-hint-copy{min-width:0; overflow:hidden; text-overflow:ellipsis; color:rgba(247,244,239,.66); font-weight:650}
.hr-hint-action,.hr-hint-keyword{
  height:25px; padding:0 10px; border-radius:999px; cursor:pointer;
  color:#f5f2ee;
  font:760 11.5px/1 'Hanken Grotesk',system-ui,sans-serif;
  transition:background-color 100ms ease-out, border-color 100ms ease-out, transform 100ms ease-out;
}
.hr-hint-keyword{display:inline-flex;align-items:center;cursor:default;border:0;background:transparent}
/* Resting state needs a visible surface — transparent+no-border read as plain
   text merged into the black strip (Adrian, 2026-08-02). */
.hr-hint-action{border:1px solid rgba(255,255,255,.18); background:rgba(255,255,255,.06)}
.hr-hint-action:hover{background:rgba(255,86,48,.22); border-color:rgba(255,86,48,.55); color:#fff}
.hr-hint-action:active,.hr-hint-close:active{transform:scale(.97)}
.hr-hint-action:focus-visible,.hr-hint-close:focus-visible{outline:2px solid rgba(148,190,255,.52);outline-offset:-2px}
.hr-hint-close{
  width:26px; height:26px; padding:0; border:0; border-radius:50%; cursor:pointer;
  display:inline-grid; place-items:center; color:#999; background:transparent;
  transition:background-color 100ms ease-out, color 100ms ease-out, transform 100ms ease-out;
}
.hr-hint-close:hover{background:rgba(229,103,91,.13); color:#ef7a70}
.hr-hint-close svg{width:14px; height:14px}
@keyframes hrHintStripIn{from{opacity:0;transform:scale(.965)}to{opacity:1;transform:scale(1)}}
@media (prefers-reduced-motion:reduce){.hr-hint-strip{animation:none}.hr-hint-action,.hr-hint-close{transition:none}}
`;

const WINDOW_X_GUARD = 22;
const WINDOW_TOP_GUARD = 10;

export default function HintPopup() {
  const [view, setView] = useState<HintView>({ kind: "hidden" });
  const cardRef = useRef<HTMLDivElement | null>(null);
  const hintHideTimer = useRef<number | null>(null);
  // Persistent window: the last reported size must survive re-renders and reset
  // per show, so a stale latch can't swallow a new presentation's resize.
  const lastSentRef = useRef({ w: 0, h: 0 });

  const clearTimers = useCallback(() => {
    if (hintHideTimer.current) window.clearTimeout(hintHideTimer.current);
    hintHideTimer.current = null;
  }, []);

  const dismiss = useCallback(() => {
    clearTimers();
    setView({ kind: "hidden" });
    void pillClient().dismissHint().catch(() => undefined);
  }, [clearTimers]);

  const saveScreenshotPreference = useCallback(
    async (destination: ScreenshotDestination) => {
      clearTimers();
      await pillClient().setScreenshotDestination(destination).catch(() => undefined);
      dismiss();
    },
    [clearTimers, dismiss],
  );

  const applyPayload = useCallback(
    (payload: PillHintPayload | null | undefined) => {
      if (!payload?.kind) return;
      clearTimers();
      if (payload.kind === "screenshot_prompt") {
        setView({ kind: "screenshot_prompt" });
        return;
      }
      if (payload.kind === "prompt_hint") {
        try {
          if (localStorage.getItem(dictationStorageKey("promptHintDisabled")) === "1") {
            dismiss();
            return;
          }
        } catch {
          // Best effort only.
        }
        setView({ kind: "prompt_hint" });
        hintHideTimer.current = window.setTimeout(() => dismiss(), 6200);
        return;
      }
      if (payload.kind in FEATURE_HINTS) {
        setView({ kind: payload.kind as FeatureHintKind });
        hintHideTimer.current = window.setTimeout(() => dismiss(), 8000);
      }
    },
    [clearTimers, dismiss],
  );

  const takePayload = useCallback(() => {
    void pillClient()
      .takeHintPayload()
      .then((payload) => {
        void pillClient().log("pill_hint_payload_take", `kind=${payload?.kind ?? "none"}`).catch(() => undefined);
        applyPayload(payload);
      })
      .catch(() => undefined);
  }, [applyPayload]);

  useEffect(() => {
    document.documentElement.style.background = "transparent";
    document.body.style.background = "transparent";
    document.body.style.margin = "0";
    document.body.style.overflow = "hidden";
    void pillClient().log("pill_hint_frontend_mount", `url=${window.location.href}`).catch(() => undefined);
  }, []);

  useEffect(() => {
    takePayload();
    const unlisten = listenPill("hintShow", () => {
      lastSentRef.current = { w: 0, h: 0 };
      takePayload();
    });
    const onVisibility = () => {
      if (document.visibilityState !== "hidden") takePayload();
    };
    document.addEventListener("visibilitychange", onVisibility);
    return () => {
      document.removeEventListener("visibilitychange", onVisibility);
      void unlisten.then((fn) => fn()).catch(() => undefined);
    };
  }, [takePayload]);

  useEffect(() => {
    const el = cardRef.current;
    if (!el || typeof ResizeObserver === "undefined") return;
    let raf = 0;
    const push = () => {
      raf = 0;
      const r = el.getBoundingClientRect();
      // Match MiniClipboardPopup's native sizing contract exactly: keep
      // transparent room on the top/sides for WebView2 clipping, but none on
      // the bottom, so the visible strip remains anchored to the pill top.
      const w = Math.ceil(r.width) + WINDOW_X_GUARD;
      const h = Math.ceil(r.height) + WINDOW_TOP_GUARD;
      const last = lastSentRef.current;
      if (Math.abs(w - last.w) <= 4 && Math.abs(h - last.h) <= 4) {
        void pillClient().hintReady().catch(() => undefined);
        return;
      }
      lastSentRef.current = { w, h };
      void pillClient().log("pill_hint_frontend_resize", `view=${view.kind} send=${w}x${h}`).catch(() => undefined);
      void pillClient()
        .resizeHint(w, h)
        .catch(() => undefined)
        .finally(() => {
          void pillClient().hintReady().catch(() => undefined);
        });
    };
    const ro = new ResizeObserver(() => {
      if (!raf) raf = requestAnimationFrame(push);
    });
    ro.observe(el);
    push();
    return () => {
      ro.disconnect();
      if (raf) cancelAnimationFrame(raf);
    };
  }, [view.kind]);

  if (view.kind === "hidden") return null;

  return (
    <div className="hr-pill-hint-root">
      <style>{`${PILL_CSS}\n${HINT_CSS}`}</style>
      <div ref={cardRef} className="hr-pill-hint-card">
        {view.kind === "prompt_hint" ? (
          <div className="hr-hint-strip prompt" role="status" aria-live="polite">
            <strong>Say</strong>
            <span className="hr-hint-keyword">prompt</span>
            <span className="hr-hint-copy">at the end to shape your dictation for AI</span>
            <button
              type="button"
              className="hr-hint-close"
              aria-label="Dismiss prompt tip"
              {...pillTooltipProps("pill_hint", "Dismiss prompt tip")}
              onClick={dismiss}
            >
              <X weight="bold" aria-hidden="true" />
            </button>
          </div>
        ) : view.kind === "screenshot_prompt" ? (
          <div className="hr-hint-strip screenshot" role="status" aria-live="polite">
            <strong>Save screenshot to</strong>
            <button
              type="button"
              className="hr-hint-action"
              {...pillTooltipProps("pill_hint", "Keep screenshots on the clipboard")}
              onClick={() => void saveScreenshotPreference("clipboard")}
            >
              Clipboard
            </button>
            <button
              type="button"
              className="hr-hint-action"
              {...pillTooltipProps("pill_hint", "Copy screenshots and save them to Pictures")}
              onClick={() => void saveScreenshotPreference("disk")}
            >
              Disk
            </button>
            <button
              type="button"
              className="hr-hint-close"
              aria-label="Dismiss screenshot choice"
              {...pillTooltipProps("pill_hint", "Dismiss screenshot choice")}
              onClick={dismiss}
            >
              <X weight="bold" aria-hidden="true" />
            </button>
          </div>
        ) : (
          <div className="hr-hint-strip feature" role="status" aria-live="polite">
            <strong>{withAppName(FEATURE_HINTS[view.kind].title)}</strong>
            <span className="hr-hint-copy">{withAppName(FEATURE_HINTS[view.kind].body)}</span>
            <button type="button" className="hr-hint-close" aria-label="Dismiss feature tip" {...pillTooltipProps("pill_hint", "Dismiss feature tip")} onClick={dismiss}>
              <X weight="bold" aria-hidden="true" />
            </button>
          </div>
        )}
      </div>
    </div>
  );
}
