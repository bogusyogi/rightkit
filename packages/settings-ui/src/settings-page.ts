// State and helpers a tabbed settings page keeps around `SettingsPanel`: the active tab, the search text, a deep link that
// switches tab then reveals a row, and the tab list the panel would show. Imports `react` and the panel helpers.
import { useCallback, useEffect, useRef, useState, type KeyboardEvent, type RefObject } from "react";
import { orderSections, revealSetting } from "./SettingsPanel.js";
import type { SettingsContext, SettingsSection, SettingsValues } from "./types.js";

export interface UseSettingsPageOptions {
  /** The tab shown first. */
  initialSectionId: string;
  /** A request to show one row (a palette choice, a deep link). A new object reveals again. */
  revealRequest?: { id: string };
  /** The tab that holds a row, or undefined when the row is in no tab (the request is then ignored). */
  sectionFor(settingId: string): string | undefined;
  /** Scrolls to and flashes the row inside the page element. Default `revealSetting` from `SettingsPanel`. */
  reveal?: (root: ParentNode, settingId: string) => void;
}

export interface SettingsPageState<T extends HTMLElement = HTMLDivElement> {
  activeId: string;
  setActiveId(id: string): void;
  query: string;
  setQuery(query: string): void;
  /** Attach to the element that contains the panel; reveals search inside it. */
  pageRef: RefObject<T | null>;
  /** Spread on the search `<input>`: `value`, `onChange`, and Escape clears the text. */
  searchProps: {
    value: string;
    onChange(event: { target: { value: string } }): void;
    onKeyDown(event: KeyboardEvent<HTMLInputElement>): void;
  };
  /** The tab to mark active: none while a search is typed (results span every tab). */
  tabActiveId: string;
  /** Tab click handler: clears the search, then switches tab. */
  selectTab(id: string): void;
}

/**
 * `activeId` and `query`, the reveal-request effect (switch to the row's tab, then on the next frame reveal the row; a
 * pending frame is cancelled when the request changes or the page unmounts), Escape-clears-search and tab selection.
 */
export function useSettingsPage<T extends HTMLElement = HTMLDivElement>(options: UseSettingsPageOptions): SettingsPageState<T> {
  const { initialSectionId, revealRequest, sectionFor, reveal = revealSetting } = options;
  const pageRef = useRef<T | null>(null);
  const [activeId, setActiveId] = useState(initialSectionId);
  const [query, setQuery] = useState("");
  const sectionForRef = useRef(sectionFor);
  sectionForRef.current = sectionFor;
  const revealRef = useRef(reveal);
  revealRef.current = reveal;

  useEffect(() => {
    const id = revealRequest?.id;
    if (!id) return;
    const section = sectionForRef.current(id);
    if (!section) return;
    setActiveId(section);
    const frame = window.requestAnimationFrame(() => {
      if (pageRef.current) revealRef.current(pageRef.current, id);
    });
    return () => window.cancelAnimationFrame(frame);
  }, [revealRequest]);

  const selectTab = useCallback((id: string) => {
    setQuery("");
    setActiveId(id);
  }, []);

  return {
    activeId,
    setActiveId,
    query,
    setQuery,
    pageRef,
    searchProps: {
      value: query,
      onChange: (event) => setQuery(event.target.value),
      onKeyDown: (event) => {
        if (event.key === "Escape") setQuery("");
      },
    },
    tabActiveId: query.trim() ? "" : activeId,
    selectTab,
  };
}

/**
 * The sections a tab bar lists, in panel order: those without a `visible` rule or whose rule holds. Pass the settings
 * context when a rule reads it; without one, rules are called with an empty context, so use it only when they ignore it.
 */
export function visibleTabSections(sections: readonly SettingsSection[], ctx?: SettingsContext): SettingsSection[] {
  const context = ctx ?? ({} as SettingsContext);
  return orderSections(sections.filter((section) => !section.visible || section.visible(context)));
}

/** `ctx.set` as a two-argument function, for a section's component that takes a `patch` prop. */
export function settingPatcher<K extends string, V>(ctx: SettingsContext): (key: K, value: V) => Promise<void> {
  return (key, value) => ctx.set(key, value);
}

/** The stored value as text; `fallback` when unset (null or undefined). */
export function settingString(values: SettingsValues, key: string, fallback: string): string {
  return String(values[key] ?? fallback);
}

/** The stored value when it is a string, else undefined. */
export function settingStringOrUndefined(values: SettingsValues, key: string): string | undefined {
  return typeof values[key] === "string" ? (values[key] as string) : undefined;
}

/** True only for the stored value `true`. */
export function settingFlag(values: SettingsValues, key: string): boolean {
  return values[key] === true;
}

/** The stored value as a number; `fallback` when unset (null or undefined). */
export function settingNumber(values: SettingsValues, key: string, fallback: number): number {
  return Number(values[key] ?? fallback);
}

/** The stored value cast to `T`; `fallback` when unset (null or undefined). */
export function settingOr<T>(values: SettingsValues, key: string, fallback: T): T {
  return values[key] == null ? fallback : (values[key] as T);
}
