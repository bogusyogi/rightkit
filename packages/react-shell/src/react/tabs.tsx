import { useCallback, useEffect, useId, useLayoutEffect, useRef, useState, type KeyboardEvent as ReactKeyboardEvent, type ReactNode } from "react";
import { cx } from "./ui.js";
import type { SlotClasses } from "./slots.js";

/** One tab of a {@link TabStrip}. */
export interface TabItem {
  /** Stable id, unique within the strip. */
  id: string;
  /** Visible label. */
  label: ReactNode;
  /** Plain-text name for the close button and the unsaved marker. Default: `label` when it is a string. */
  title?: string;
  /** Leading icon slot (a format icon, a favicon). */
  icon?: ReactNode;
  /** Trailing slot inside the tab, before the close button (a badge, a spinner). */
  trailing?: ReactNode;
  /** Shows the unsaved marker. */
  dirty?: boolean;
  /** Show a close button on this tab. Default: true when the strip has `onClose`. */
  closable?: boolean;
  disabled?: boolean;
  /** `id` of the tabpanel this tab controls (`aria-controls`). */
  panelId?: string;
  /** Extra class on this tab's wrapper. */
  className?: string;
}

export interface TabStripProps {
  tabs: readonly TabItem[];
  /** Id of the selected tab, or null for none. */
  activeId: string | null;
  onSelect: (id: string) => void;
  /** Close a tab (close button, middle click, Delete). Without it no tab is closable. */
  onClose?: (id: string) => void;
  /**
   * Move tab `id` to index `toIndex` of the order without it. Without it tabs do not drag and Alt+Shift+Arrow does
   * nothing. The owner re-renders `tabs` in the new order.
   */
  onReorder?: (id: string, toIndex: number) => void;
  /** Accessible name of the tablist. */
  label: string;
  /** Arrow keys move focus only (`"manual"`, Enter or Space selects) or select as they move (`"automatic"`). Default `"automatic"`. */
  activation?: "automatic" | "manual";
  /** Slot after the scroller (a new-tab button, a tab overview). It stays visible while the tabs scroll. */
  trailing?: ReactNode;
  /** Slot before the scroller. */
  leading?: ReactNode;
  /** Render scroll arrows when the tabs overflow. Default true. */
  scrollArrows?: boolean;
  /** Names of the scroll buttons. */
  scrollLeftLabel?: string;
  scrollRightLabel?: string;
  /** Builds the close button name from the tab title. Default `Close <title>`. */
  closeLabel?: (title: string) => string;
  /** Name of the unsaved marker. Default `Unsaved changes`. */
  dirtyLabel?: string;
  /** Replaces the label area of a tab (icon, label and dirty marker); the tab button, close button and roles stay. */
  renderTabContent?: (tab: TabItem, state: { active: boolean }) => ReactNode;
  className?: string;
  /** Extra classes per region: `root`, `scroller`, `list`, `tab`, `tabButton`, `icon`, `label`, `dirty`, `close`, `arrow`, `leading`, `trailing`. */
  slots?: SlotClasses<"root" | "scroller" | "list" | "tab" | "tabButton" | "icon" | "label" | "dirty" | "close" | "arrow" | "leading" | "trailing">;
}

const textOf = (tab: TabItem) => tab.title ?? (typeof tab.label === "string" ? tab.label : tab.id);

/**
 * Horizontal tab strip: `tablist` and `tab` roles, roving tabindex (Left/Right/Home/End, Delete closes, Alt+Shift+Arrow
 * reorders), per-tab close button and unsaved marker, overflow scroll arrows, and drag reorder. Controlled: the owner
 * holds `tabs`, `activeId` and the order. Structure only; the look (the fused curve, colours) belongs to the app's CSS
 * on the `rk-tab*` classes. The tabpanel stays the owner's, wired through `TabItem.panelId`.
 */
export function TabStrip({
  tabs,
  activeId,
  onSelect,
  onClose,
  onReorder,
  label,
  activation = "automatic",
  trailing,
  leading,
  scrollArrows = true,
  scrollLeftLabel = "Scroll tabs left",
  scrollRightLabel = "Scroll tabs right",
  closeLabel = (title) => `Close ${title}`,
  dirtyLabel = "Unsaved changes",
  renderTabContent,
  className,
  slots,
}: TabStripProps) {
  const baseId = useId();
  const scrollerRef = useRef<HTMLDivElement>(null);
  const listRef = useRef<HTMLDivElement>(null);
  const [overflow, setOverflow] = useState({ left: false, right: false });
  const [dragId, setDragId] = useState<string | null>(null);
  const [drop, setDrop] = useState<{ id: string; after: boolean } | null>(null);

  const measure = useCallback(() => {
    const el = scrollerRef.current;
    if (!el) return;
    const left = el.scrollLeft > 1;
    const right = el.scrollLeft + el.clientWidth < el.scrollWidth - 1;
    setOverflow((prev) => (prev.left === left && prev.right === right ? prev : { left, right }));
  }, []);

  useLayoutEffect(() => {
    measure();
  }, [measure, tabs]);

  useEffect(() => {
    const el = scrollerRef.current;
    if (!el) return;
    el.addEventListener("scroll", measure, { passive: true });
    const ro = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(measure);
    ro?.observe(el);
    if (listRef.current) ro?.observe(listRef.current);
    return () => {
      el.removeEventListener("scroll", measure);
      ro?.disconnect();
    };
  }, [measure]);

  // Keep the selected tab in view.
  useEffect(() => {
    if (activeId === null) return;
    const el = listRef.current?.querySelector<HTMLElement>(`[data-tab-id="${CSS.escape(activeId)}"]`);
    el?.scrollIntoView?.({ block: "nearest", inline: "nearest" });
  }, [activeId]);

  const scrollBy = (dir: -1 | 1) => {
    const el = scrollerRef.current;
    if (el) el.scrollBy({ left: dir * Math.max(80, el.clientWidth * 0.8), behavior: "smooth" });
  };

  const enabledIds = tabs.filter((t) => !t.disabled).map((t) => t.id);
  const tabbable = activeId !== null && enabledIds.includes(activeId) ? activeId : (enabledIds[0] ?? null);
  const closableFor = (tab: TabItem) => Boolean(onClose) && tab.closable !== false;

  const focusTab = (id: string) => listRef.current?.querySelector<HTMLElement>(`[data-tab-button="${CSS.escape(id)}"]`)?.focus();

  const onKeyDown = (event: ReactKeyboardEvent<HTMLButtonElement>, tab: TabItem) => {
    if (event.nativeEvent.isComposing) return;
    const at = enabledIds.indexOf(tab.id);
    const order = tabs.findIndex((t) => t.id === tab.id);
    if (event.altKey && event.shiftKey && onReorder && (event.key === "ArrowLeft" || event.key === "ArrowRight")) {
      event.preventDefault();
      const to = event.key === "ArrowLeft" ? order - 1 : order + 1;
      if (to >= 0 && to < tabs.length) {
        onReorder(tab.id, to);
        requestAnimationFrame(() => focusTab(tab.id));
      }
      return;
    }
    if (event.ctrlKey || event.metaKey || event.altKey) return;
    let next: string | undefined;
    if (event.key === "ArrowRight") next = enabledIds[(at + 1) % enabledIds.length];
    else if (event.key === "ArrowLeft") next = enabledIds[(at - 1 + enabledIds.length) % enabledIds.length];
    else if (event.key === "Home") next = enabledIds[0];
    else if (event.key === "End") next = enabledIds[enabledIds.length - 1];
    else if (event.key === "Delete" && closableFor(tab)) {
      event.preventDefault();
      const neighbour = enabledIds[at + 1] ?? enabledIds[at - 1];
      onClose?.(tab.id);
      if (neighbour) requestAnimationFrame(() => focusTab(neighbour));
      return;
    } else return;
    event.preventDefault();
    if (next === undefined) return;
    focusTab(next);
    if (activation === "automatic") onSelect(next);
  };

  const endDrag = () => {
    setDragId(null);
    setDrop(null);
  };

  const commitDrop = () => {
    if (dragId && drop && onReorder) {
      const rest = tabs.filter((t) => t.id !== dragId);
      const target = rest.findIndex((t) => t.id === drop.id);
      if (target >= 0) {
        const to = drop.after ? target + 1 : target;
        const from = tabs.findIndex((t) => t.id === dragId);
        if (to !== from) onReorder(dragId, to);
      }
    }
    endDrag();
  };

  return (
    <div className={cx("rk-tabs", className, slots?.root)} style={{ display: "flex", alignItems: "stretch", minWidth: 0 }}>
      {leading ? <div className={cx("rk-tabs__leading", slots?.leading)}>{leading}</div> : null}
      {scrollArrows && overflow.left ? (
        <button type="button" tabIndex={-1} aria-label={scrollLeftLabel} className={cx("rk-tabs__arrow rk-tabs__arrow--left", slots?.arrow)} onClick={() => scrollBy(-1)}>
          <svg viewBox="0 0 16 16" width="14" height="14" aria-hidden="true" focusable="false"><path d="M10 3 5 8l5 5" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" /></svg>
        </button>
      ) : null}
      <div ref={scrollerRef} className={cx("rk-tabs__scroller", slots?.scroller)} style={{ flex: "1 1 auto", minWidth: 0, overflowX: "auto", overflowY: "hidden", scrollbarWidth: "none" }}>
        <div ref={listRef} role="tablist" aria-label={label} aria-orientation="horizontal" className={cx("rk-tabs__list", slots?.list)} style={{ display: "flex", width: "max-content" }}>
          {tabs.map((tab) => {
            const active = tab.id === activeId;
            const title = textOf(tab);
            const dropping = drop?.id === tab.id && dragId !== tab.id;
            return (
              <div
                key={tab.id}
                role="presentation"
                data-tab-id={tab.id}
                className={cx("rk-tab", active && "is-active", tab.dirty && "is-dirty", tab.disabled && "is-disabled", dragId === tab.id && "is-dragging", dropping && (drop?.after ? "is-drop-after" : "is-drop-before"), tab.className, slots?.tab)}
                style={{ flex: "0 0 auto", display: "flex", alignItems: "center" }}
                draggable={Boolean(onReorder) && !tab.disabled}
                onDragStart={(e) => {
                  if (!onReorder) return;
                  e.dataTransfer.effectAllowed = "move";
                  e.dataTransfer.setData("text/plain", tab.id);
                  setDragId(tab.id);
                }}
                onDragOver={(e) => {
                  if (!dragId) return;
                  e.preventDefault();
                  e.dataTransfer.dropEffect = "move";
                  const rect = e.currentTarget.getBoundingClientRect();
                  const after = e.clientX > rect.left + rect.width / 2;
                  setDrop((prev) => (prev?.id === tab.id && prev.after === after ? prev : { id: tab.id, after }));
                }}
                onDrop={(e) => {
                  if (!dragId) return;
                  e.preventDefault();
                  commitDrop();
                }}
                onDragEnd={endDrag}
                onAuxClick={(e) => {
                  if (e.button === 1 && closableFor(tab)) {
                    e.preventDefault();
                    onClose?.(tab.id);
                  }
                }}
                onMouseDown={(e) => {
                  // Middle button would start autoscroll on some platforms.
                  if (e.button === 1 && closableFor(tab)) e.preventDefault();
                }}
              >
                <button
                  type="button"
                  role="tab"
                  id={`${baseId}-tab-${tab.id}`}
                  data-tab-button={tab.id}
                  aria-selected={active}
                  aria-controls={tab.panelId}
                  aria-disabled={tab.disabled || undefined}
                  disabled={tab.disabled}
                  tabIndex={tab.id === tabbable ? 0 : -1}
                  title={title}
                  className={cx("rk-tab__button", slots?.tabButton)}
                  onClick={() => onSelect(tab.id)}
                  onKeyDown={(e) => onKeyDown(e, tab)}
                >
                  {renderTabContent ? (
                    renderTabContent(tab, { active })
                  ) : (
                    <>
                      {tab.icon ? <span className={cx("rk-tab__icon", slots?.icon)} aria-hidden="true">{tab.icon}</span> : null}
                      <span className={cx("rk-tab__label", slots?.label)}>{tab.label}</span>
                      {tab.dirty ? <span className={cx("rk-tab__dirty", slots?.dirty)} role="img" aria-label={dirtyLabel} /> : null}
                    </>
                  )}
                </button>
                {tab.trailing}
                {closableFor(tab) ? (
                  <button type="button" tabIndex={-1} aria-label={closeLabel(title)} className={cx("rk-tab__close", slots?.close)} onClick={() => onClose?.(tab.id)}>
                    <svg viewBox="0 0 16 16" width="12" height="12" aria-hidden="true" focusable="false"><path d="m4 4 8 8M12 4l-8 8" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" /></svg>
                  </button>
                ) : null}
              </div>
            );
          })}
        </div>
      </div>
      {scrollArrows && overflow.right ? (
        <button type="button" tabIndex={-1} aria-label={scrollRightLabel} className={cx("rk-tabs__arrow rk-tabs__arrow--right", slots?.arrow)} onClick={() => scrollBy(1)}>
          <svg viewBox="0 0 16 16" width="14" height="14" aria-hidden="true" focusable="false"><path d="m6 3 5 5-5 5" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" /></svg>
        </button>
      ) : null}
      {trailing ? <div className={cx("rk-tabs__trailing", slots?.trailing)}>{trailing}</div> : null}
    </div>
  );
}
