// Drives @rightkit/platform-ui/headless hooks the way ViewRight's own markup would: no app-shell component is
// used. Every option comes from the query string so one page covers each combination.
import { StrictMode, useEffect, useRef, useState } from "react";
import { createRoot } from "react-dom/client";
import {
  fitToViewport,
  rankCommands,
  useDismiss,
  useExitTransition,
  useFocusTrap,
  useListNavigation,
  type FocusTrapOptions,
  type OutsidePress,
} from "@rightkit/platform-ui/headless";

const q = new URLSearchParams(location.search);
const demo = q.get("demo") ?? "menu";
const flag = (k: string) => q.get(k) === "1";
const log = (line: string) => {
  const out = document.getElementById("log");
  if (out) out.textContent += `${line}\n`;
};
window.addEventListener("resize", () => log("win-resize"));
// Observers that sit beyond the hook, to see what Escape does to propagation and default action. An unprevented
// Escape sent through DevTools input stalls later frames in headless Chrome, so every observer prevents it after
// logging what the hook left behind.
document.addEventListener("keydown", (e) => {
  if (e.key !== "Escape") return;
  log(`doc-escape prevented=${e.defaultPrevented}`);
  e.preventDefault();
});
/** Register after the dismiss hook (effects run in order), on the same target and phase, so it runs after the hook's listener. */
function useEscapeObserver() {
  useEffect(() => {
    const w = window as unknown as { __observerMounts?: number };
    w.__observerMounts = (w.__observerMounts ?? 0) + 1; // 2 for one mount proves StrictMode double-invoked the effects
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      log(`win-escape prevented=${e.defaultPrevented}`);
      e.preventDefault();
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, []);
}

function Menu() {
  const [at, setAt] = useState<{ x: number; y: number; n: number } | null>(null);
  const opens = useRef(0);
  return (
    <>
      <div style={{ padding: 24, minHeight: "100vh", boxSizing: "border-box" }} onContextMenu={(e) => { e.preventDefault(); setAt({ x: e.clientX, y: e.clientY, n: ++opens.current }); }}>
        <div id="area" style={{ height: 300, background: "#eee" }}>right-click</div>
        <button type="button" id="page-btn" onClick={() => log("page:btn")}>page</button>
      </div>
      {at ? <MenuLayer key={flag("inplace") ? 0 : at.n} x={at.x} y={at.y} onGone={() => { log("menu:gone"); setAt(null); }} /> : null}
    </>
  );
}

function MenuLayer({ x, y, onGone }: { x: number; y: number; onGone: () => void }) {
  const panel = useRef<HTMLDivElement>(null);
  const scrim = useRef<HTMLDivElement>(null);
  const exit = useExitTransition({
    exitMs: Number(q.get("exit") ?? 80),
    closeAfter: (q.get("after") as "immediately" | "settled" | null) ?? "immediately",
    onClosed: onGone,
    onExitStart: () => log("menu:exit-start"),
  });
  (window as unknown as { __exit: typeof exit }).__exit = exit;
  const outside = (q.get("dismiss") ?? "mousedown") as OutsidePress;
  useDismiss(() => void exit.close(), {
    enabled: exit.isOpen,
    container: panel,
    backdrop: q.get("backdrop") === "0" ? undefined : scrim,
    outside,
    outsideContextMenu: true,
    preventDefault: false,
    stopPropagation: false,
    imeGuard: false,
    onWindowBlur: flag("blur"),
    onResize: flag("resize"),
  });
  useEscapeObserver();
  const pos = fitToViewport({ x, y, estimatedSize: { width: 176, height: 260 }, viewport: { width: window.innerWidth, height: window.innerHeight } });
  const items = ["Cut", "Copy", "Paste"];
  const run = (label: string) => () => {
    log(`menu:run ${label}`);
    if (q.get("after") === "settled") return new Promise<void>((r) => { (window as unknown as { __release: () => void }).__release = r; });
    return undefined;
  };
  return (
    <>
      <div ref={scrim} id="scrim" style={{ position: "fixed", inset: 0, zIndex: 60 }} />
      <div
        ref={panel}
        role="menu"
        aria-label="Document actions"
        data-closing={exit.closing}
        data-pending={exit.pending}
        onMouseDown={(e) => e.preventDefault()}
        style={{ position: "fixed", zIndex: 61, background: "#fff", border: "1px solid #999", boxSizing: "border-box", left: pos.left, top: pos.top, width: pos.width, maxHeight: pos.maxHeight, opacity: exit.closing ? 0.4 : 1 }}
      >
        {items.map((label) => (
          <button key={label} type="button" role="menuitem" style={{ display: "block", width: "100%", height: 28 }} onClick={() => void exit.close(run(label)).catch(() => undefined)}>{label}</button>
        ))}
      </div>
    </>
  );
}

function Modal() {
  const [open, setOpen] = useState(false);
  return (
    <div style={{ padding: 24 }}>
      <input id="before" defaultValue="before" />
      <button type="button" id="opener" onClick={() => setOpen(true)}>Open dialog</button>
      <button type="button" id="after" onClick={() => log("after:click")}>after</button>
      {open ? <ModalLayer onGone={() => { log("modal:gone"); setOpen(false); }} /> : null}
    </div>
  );
}

function ModalLayer({ onGone }: { onGone: () => void }) {
  const overlay = useRef<HTMLDivElement>(null);
  const panel = useRef<HTMLDivElement>(null);
  const exit = useExitTransition({ exitMs: 80, onClosed: onGone });
  const restore = q.get("restore");
  const trapOpts: FocusTrapOptions = {
    captureOpener: (q.get("capture") as "render" | "effect" | null) ?? undefined,
    restoreTiming: restore === "sync" ? "sync" : restore && /^\d+$/.test(restore) ? Number(restore) : undefined,
    restore: restore === "none" ? false : undefined,
    fromOutside: (q.get("from") as "both" | "backward" | "none" | null) ?? undefined,
    skipHidden: q.get("skipHidden") === "0" ? false : undefined,
    emptyList: (q.get("emptyList") as "prevent" | "pass" | null) ?? undefined,
    focusFallback: (q.get("fallback") as "container" | "none" | null) ?? undefined,
    trap: q.get("trap") === "0" ? false : undefined,
    autoFocus: q.get("autoFocus") === "0" ? false : undefined,
  };
  useFocusTrap(panel, true, trapOpts);
  useDismiss(() => void exit.close(), {
    enabled: exit.isOpen,
    container: panel,
    backdrop: overlay,
    outside: (q.get("outside") as OutsidePress | null) ?? "mousedown",
    preventDefault: q.get("pd") === "0" ? false : undefined,
    stopPropagation: q.get("sp") === "0" ? false : undefined,
  });
  useEscapeObserver();
  return (
    <div ref={overlay} id="overlay" style={{ position: "fixed", inset: 0, background: "rgba(0,0,0,.4)", display: "flex", alignItems: "center", justifyContent: "center", opacity: exit.closing ? 0.2 : 1 }}>
      <div ref={panel} role="dialog" aria-modal="true" aria-label="Confirm" tabIndex={-1} style={{ background: "#fff", padding: 20, width: 320 }}>
        {flag("empty") ? <p>Nothing to focus</p> : (
          <>
            {flag("hidden") ? <button type="button" id="hid" hidden>Hidden</button> : null}
            <input id="first" defaultValue="first" autoFocus={flag("raf")} />
            <button type="button" id="mid">middle</button>
            <button type="button" id="last" onClick={() => void exit.close()}>Done</button>
          </>
        )}
      </div>
    </div>
  );
}

const COMMANDS = Array.from({ length: 24 }, (_, i) => ({ id: `c${i}`, label: i < 3 ? ["Open file", "Open URL", "Reopen closed tab"][i] : `Command ${i}` }));

function Palette() {
  const [open, setOpen] = useState(false);
  return (
    <div style={{ padding: 24 }}>
      <button type="button" id="opener" onClick={() => setOpen(true)}>Open palette</button>
      {open ? <PaletteLayer onGone={() => { log("palette:gone"); setOpen(false); }} /> : null}
    </div>
  );
}

function PaletteLayer({ onGone }: { onGone: () => void }) {
  const [query, setQuery] = useState("");
  const overlay = useRef<HTMLDivElement>(null);
  const dialog = useRef<HTMLDivElement>(null);
  const exit = useExitTransition({ exitMs: 80, onClosed: onGone });
  const items = rankCommands(COMMANDS, query, 50);
  const vr = q.get("profile") !== "app";
  const nav = useListNavigation({
    count: items.length,
    wrap: vr ? false : true,
    homeEnd: vr ? false : true,
    keepIndexOnItemsChange: vr,
    resetKey: vr ? undefined : query,
    hoverSelects: vr ? "always" : "after-pointer-move",
    idPrefix: "vr-palette",
    listId: "vr-palette-results",
    optionId: (i) => `vr-palette-option-${i}`,
    scrollIntoView: true,
    onActivate: (i) => void exit.close(() => log(`run ${items[i].label}`)),
  });
  useFocusTrap(dialog, true, { autoFocus: false });
  useDismiss(() => void exit.close(), { enabled: exit.isOpen, escape: true, escapeTarget: dialog, stopPropagation: false, container: dialog, backdrop: overlay, outside: "click-target" });
  return (
    <div ref={overlay} id="overlay" style={{ position: "fixed", inset: 0, background: "rgba(0,0,0,.4)", paddingTop: 80, display: "flex", justifyContent: "center", alignItems: "flex-start" }}>
      <div ref={dialog} role="dialog" aria-modal="true" aria-label="Command palette" style={{ background: "#fff", width: 420 }}>
        <input id="q" autoFocus value={query} onChange={(e) => setQuery(e.target.value)} aria-label="Search" placeholder="Search" {...nav.getInputProps()} />
        <div {...nav.getListProps()} style={{ maxHeight: 200, overflow: "auto" }}>
          {items.length === 0 ? <div id="empty">No matches.</div> : null}
          {items.map((it, i) => (
            <button key={it.id} type="button" tabIndex={-1} data-active={i === nav.activeIndex} style={{ display: "block", width: "100%", height: 30, textAlign: "left" }} {...nav.getOptionProps(i)} onClick={() => void exit.close(() => log(`run ${it.label}`))}>
              {it.label}
            </button>
          ))}
        </div>
      </div>
    </div>
  );
}

const app = demo === "modal" ? <Modal /> : demo === "palette" ? <Palette /> : <Menu />;
createRoot(document.getElementById("root")!).render(flag("strict") ? <StrictMode>{app}</StrictMode> : app);
(window as unknown as { __ready: boolean }).__ready = true;
