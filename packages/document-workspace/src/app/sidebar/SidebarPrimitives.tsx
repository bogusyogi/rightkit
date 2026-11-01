// Shared small presentational pieces reused by the sidebar's popovers (the tab overview) and by the collapsed rail / expanded header icon buttons.
// Extracted verbatim from Sidebar.tsx as part of the 2026-07-02 decomposition plan (V-K).
import { useDismiss, useExitTransition, useFocusTrap, useListNavigation } from "@rightkit/platform-ui/headless";
import { useEffect, useLayoutEffect, useRef, useState } from "react";

const menuItems = (root: HTMLElement | null) => [
  ...(root?.querySelectorAll<HTMLElement>('[role="menuitem"]:not([disabled])') ?? []),
];

export const menuItemClass =
  "vr-press vr-focus flex w-full items-center px-3 py-1.5 text-left font-sans text-sm text-paper/85 hover:bg-ink/50 hover:text-paper focus-visible:bg-ink/50 focus-visible:text-paper";

export const compactMenuItemClass =
  "vr-press vr-focus flex w-full items-center px-2.5 py-1 text-left font-sans text-xs text-paper/85 hover:bg-ink/50 hover:text-paper focus-visible:bg-ink/50 focus-visible:text-paper";

/** Classes every shell-menu panel in the sidebar shares (the glass panel; position comes from the shell). */
export const menuPanelClass = "vr-pop no-print vr-glass-panel overflow-hidden rounded-lg";

export function Popover({
  onClose,
  className,
  children,
  role = "menu",
  ariaModal,
  ariaLabel,
}: {
  onClose: () => void;
  className: string;
  children: React.ReactNode;
  role?: "menu" | "dialog";
  ariaModal?: boolean;
  ariaLabel?: string;
}) {
  const panelRef = useRef<HTMLDivElement>(null);
  const backdropRef = useRef<HTMLDivElement>(null);
  const exit = useExitTransition({ exitMs: 80, onClosed: onClose });
  const close = () => void exit.close();
  // Escape and backdrop press dismiss; focus lands on the first item and returns to the opener.
  useDismiss(close, { outside: "click-target", backdrop: backdropRef });
  useFocusTrap(panelRef, true, { autoFocus: false, trap: false });
  const [count, setCount] = useState(0);
  useLayoutEffect(() => {
    const n = menuItems(panelRef.current).length;
    if (n !== count) setCount(n);
  });
  const nav = useListNavigation({
    count,
    role: "menu",
    focusOwner: "list",
    homeEnd: false,
    activateKeys: [],
    keepIndexOnItemsChange: true,
  });
  // biome-ignore lint/correctness/useExhaustiveDependencies: re-run once the items are counted so the first item takes focus on open
  useEffect(() => {
    menuItems(panelRef.current)[nav.activeIndex]?.focus();
  }, [nav.activeIndex, count]);
  const { onKeyDown } = nav.getListProps();
  return (
    <>
      <div ref={backdropRef} className="no-print fixed inset-0 z-30" aria-hidden="true" />
      <div
        ref={panelRef}
        onKeyDown={onKeyDown}
        onFocus={(event) => {
          const at = menuItems(panelRef.current).indexOf(event.target as HTMLElement);
          if (at >= 0) nav.setActiveIndex(at);
        }}
        role={role}
        {...(role === "dialog" ? { "aria-modal": ariaModal, "aria-label": ariaLabel } : {})}
        className={`vr-pop vr-pop-menu-tr${exit.closing ? " vr-pop-exit" : ""} no-print vr-glass-panel absolute z-40 overflow-hidden rounded-lg py-1 ${className}`}
      >
        {children}
      </div>
    </>
  );
}


