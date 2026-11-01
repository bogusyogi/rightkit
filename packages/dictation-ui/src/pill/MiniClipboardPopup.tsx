import { useDismiss } from "@rightkit/platform-ui/headless";
import { useCallback, useEffect, useRef, useState } from "react";
import { invoke, listen } from "../lib/tauri.js";
import { pillTooltipProps } from "./pillTooltip.js";
import MiniClipboard, {
  MINI_CLIPBOARD_CSS,
  type MiniClipboardItem,
  type MiniClipboardMode,
} from "./MiniClipboard.js";

const CSS = `
html,body,#root{margin:0;width:100%;height:100%;background:transparent;overflow:hidden}
.hr-mini-clip-root{
  --edge:rgba(255,255,255,.16); --coffee:rgba(62,60,56,.86); --coffee-2:rgba(255,255,255,.08);
  --fg:rgba(247,244,239,.94); --muted:rgba(247,244,239,.58);
  box-sizing:border-box; width:100vw; height:100vh; padding:10px 11px 0;
  font-family:'Hanken Grotesk',system-ui,-apple-system,sans-serif; color:var(--fg);
  -webkit-user-select:none; user-select:none;
  display:flex; align-items:flex-end; justify-content:center;
  position:relative;
}
.hr-mini-clip-backdrop{position:absolute; inset:0; border:0; background:transparent; padding:0}
.hr-mini-clip-card{position:relative; z-index:1}
.hr-mini-clip-root .hr-clip{margin-bottom:0}
`;

const WINDOW_X_GUARD = 22;
const WINDOW_TOP_GUARD = 10;

export default function MiniClipboardPopup() {
  const [items, setItems] = useState<MiniClipboardItem[]>([]);
  const [loaded, setLoaded] = useState(false);
  const [mode, setMode] = useState<MiniClipboardMode>("recent");
  const [closed, setClosed] = useState(false);
  // The single/fallback popup auto-dismisses after 5s ONLY if the user never
  // touches it. The first real interaction (keystroke or click inside) cancels
  // that for good — after which it closes only via Copy, the close button, or a
  // click outside (Rust-side poll). Programmatic mount-focus does NOT count.
  const [interacted, setInteracted] = useState(false);
  const cardRef = useRef<HTMLDivElement | null>(null);
  const revealReadyRef = useRef(false);
  const dismissedRef = useRef(false);
  // Last size reported to Rust. Refs, not closure locals: the window is
  // persistent, so these must survive re-renders AND be resettable per show —
  // otherwise a stale latch can suppress the resize that a new presentation
  // needs (the sub-4px guard below would swallow it).
  const lastSentRef = useRef({ w: 0, h: 0 });

  const dismiss = useCallback(() => {
    dismissedRef.current = true;
    setClosed(true);
    void invoke("dismiss_pill_clipboard").catch(() => undefined);
  }, []);

  // Load (or reload) recent items. The popup is ephemeral, but the watchdog may
  // re-emit clipboard://show while the window is still alive and waiting for ready.
  const loadItems = useCallback(() => {
    setLoaded(false);
    void invoke<MiniClipboardItem[]>("get_pill_clipboard_items")
      .then((rows) => {
        const next = Array.isArray(rows) ? rows.slice(0, 3) : [];
        setItems(next);
        setMode(next.length === 1 && next[0]?.delivery_id === "current" ? "single" : "recent");
      })
      .catch(() => {
        setItems([]);
        setMode("recent");
      })
      .finally(() => setLoaded(true));
  }, []);

  useEffect(() => {
    dismissedRef.current = false;
    setClosed(false);
    loadItems();
    const un = listen<unknown>("clipboard://show", () => {
      // The window is persistent (hidden on dismiss, reused on show), so each
      // show is a fresh presentation: clear the previous dismissal/interaction
      // instead of ignoring the event.
      dismissedRef.current = false;
      setInteracted(false);
      setClosed(false);
      lastSentRef.current = { w: 0, h: 0 };
      loadItems();
    });
    return () => {
      void un.then((f) => f()).catch(() => undefined);
    };
  }, [loadItems]);

  // Window auto-size: the popup window is created at a fixed size, so taller
  // content (more rows / longer transcripts) would clip at the top (the content
  // is bottom-anchored). Report the card's content box to Rust so the window
  // hugs it and grows upward. Mirrors PillOverlay's ResizeObserver.
  useEffect(() => {
    const el = cardRef.current;
    if (!el || typeof ResizeObserver === "undefined") return;
    let raf = 0;
    const push = () => {
      raf = 0;
      const r = el.getBoundingClientRect();
      // WebView2 can clip rounded cards that are resized to their exact border
      // box. Keep transparent room on the top/sides, but none on the bottom, so
      // the visible card still anchors directly to the pill top.
      const w = Math.ceil(r.width) + WINDOW_X_GUARD;
      const h = Math.ceil(r.height) + WINDOW_TOP_GUARD;
      // Ignore sub-4px deltas: micro relayouts (e.g. the Copy→Copied button
      // swap) would otherwise re-park the whole window, reading as the popup
      // "shifting" on copy.
      const last = lastSentRef.current;
      if (Math.abs(w - last.w) <= 4 && Math.abs(h - last.h) <= 4) return;
      lastSentRef.current = { w, h };
      void invoke("pill_clipboard_resize", { width: w, height: h })
        .catch(() => undefined)
        .finally(() => {
          if (revealReadyRef.current) {
            void invoke("pill_clipboard_ready").catch(() => undefined);
          }
        });
    };
    const ro = new ResizeObserver(() => {
      if (!raf) raf = requestAnimationFrame(push);
    });
    ro.observe(el);
    return () => {
      ro.disconnect();
      if (raf) cancelAnimationFrame(raf);
    };
  }, []);

  useEffect(() => {
    document.documentElement.style.background = "transparent";
    document.body.style.background = "transparent";
  }, []);

  // Never leave an empty popup on screen. A transparent WebView with no rows is
  // still an input window, so it becomes an invisible blocker above the pill.
  useEffect(() => {
    if (loaded && items.length === 0) dismiss();
  }, [loaded, items.length, dismiss]);

  // Native keeps the popup hidden/click-through until React has loaded content
  // and reported its measured card size; the ResizeObserver then sends ready.
  useEffect(() => {
    revealReadyRef.current = loaded && !closed && items.length > 0;
    if (revealReadyRef.current && cardRef.current) {
      requestAnimationFrame(() => {
        const el = cardRef.current;
        if (!revealReadyRef.current || !el) return;
        const r = el.getBoundingClientRect();
        const w = Math.ceil(r.width) + WINDOW_X_GUARD;
        const h = Math.ceil(r.height) + WINDOW_TOP_GUARD;
        lastSentRef.current = { w, h };
        void invoke("pill_clipboard_resize", { width: w, height: h })
          .catch(() => undefined)
          .finally(() => {
            if (revealReadyRef.current) {
              void invoke("pill_clipboard_ready").catch(() => undefined);
            }
          });
      });
    }
  }, [loaded, closed, items.length]);

  useDismiss(() => dismiss(), { outside: false, preventDefault: false, stopPropagation: false });

  // Reset the interaction latch whenever a fresh single popup opens. mode+items
  // ARE the "fresh popup" signal — biome flags them as unnecessary because the
  // body only calls a stable setter, but the deps are the intended trigger.
  // biome-ignore lint/correctness/useExhaustiveDependencies: reset is intentionally keyed to mode/items changes
  useEffect(() => {
    setInteracted(false);
  }, [mode, items]);

  // Editing cancels auto-dismiss. Plain pointer presses do not: a failed close
  // click must not also remove the timer that would recover the popup.
  useEffect(() => {
    if (mode !== "single") return;
    const mark = (event: Event) => {
      const target = event.target;
      if (target instanceof HTMLTextAreaElement || target instanceof HTMLInputElement) {
        setInteracted(true);
      }
    };
    window.addEventListener("keydown", mark, { once: true });
    window.addEventListener("input", mark, { once: true });
    return () => {
      window.removeEventListener("keydown", mark);
      window.removeEventListener("input", mark);
    };
  }, [mode]);

  // Auto-dismiss 5s after open — but only while untouched. Once `interacted`, the
  // popup stays until Copy / close / click-outside.
  useEffect(() => {
    if (mode !== "single" || interacted) return;
    const id = window.setTimeout(dismiss, 5000);
    return () => window.clearTimeout(id);
  }, [mode, interacted, dismiss]);

  if (closed) return null;

  return (
    <div className="hr-mini-clip-root">
      <style>{`${CSS}\n${MINI_CLIPBOARD_CSS}`}</style>
      <button
        type="button"
        className="hr-mini-clip-backdrop"
        aria-label="Close recent transcripts"
        {...pillTooltipProps("pill_clipboard", "Close recent transcripts")}
        onClick={dismiss}
      />
      <div ref={cardRef} className="hr-mini-clip-card" style={{ display: "inline-flex" }}>
        <MiniClipboard
          items={items}
          loaded={loaded}
          mode={mode}
          onModeChange={(nextMode, nextItems) => {
            setMode(nextMode);
            setItems(nextItems);
          }}
          onClose={dismiss}
          onAfterOpenHub={dismiss}
          onEditInteraction={() => setInteracted(true)}
          closeAfterCopy
        />
      </div>
    </div>
  );
}
