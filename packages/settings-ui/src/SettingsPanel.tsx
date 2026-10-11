import { useEffect, useLayoutEffect, useMemo, useRef, useState, type KeyboardEvent, type ReactNode } from "react";
import { createMemorySettings } from "./backends.js";
import { cx } from "./primitives.js";
import {
  SettingsResultsAnnouncer,
  SettingsSearchProvider,
  SettingsSearchSection,
  SettingsSectionScope,
  defaultAnnouncement,
  useSearchKeys,
  useSettingsSearch,
  type EscapeClears,
  type SettingsHighlight,
} from "./search.js";
import type { SettingsBackend, SettingsContext, SettingsSection, SettingsSnapshot } from "./types.js";
import { useSettings } from "./useSettings.js";

/** Sort by group (first appearance), then `order`, then label. Sections an app adds slot in by `order`. */
export function orderSections(sections: readonly SettingsSection[]): SettingsSection[] {
  const groups: string[] = [];
  for (const s of sections) if (!groups.includes(s.group ?? "")) groups.push(s.group ?? "");
  const first = new Map(groups.map((group, index) => [group, index]));
  const groupOrder = new Map(groups.map((group) => [group, Math.min(...sections.filter((s) => (s.group ?? "") === group).map((s) => s.order ?? 1000))]));
  return [...sections].sort((a, b) => groupOrder.get(a.group ?? "")! - groupOrder.get(b.group ?? "")! || first.get(a.group ?? "")! - first.get(b.group ?? "")! || (a.order ?? 1000) - (b.order ?? 1000) || a.label.localeCompare(b.label));
}

/**
 * Combine the standard sections with app sections. An app section with the same id replaces the standard one;
 * `hide` drops standard sections the app does not want. Duplicate ids otherwise throw so a typo never silently shadows a section.
 */
export function mergeSections(standard: readonly SettingsSection[], custom: readonly SettingsSection[] = [], opts: { hide?: readonly string[] } = {}): SettingsSection[] {
  const hide = new Set(opts.hide ?? []);
  const customIds = new Set(custom.map((s) => s.id));
  if (customIds.size !== custom.length) throw new Error("mergeSections: duplicate ids in custom sections");
  return [...standard.filter((s) => !hide.has(s.id) && !customIds.has(s.id)), ...custom];
}

const norm = (s: string) => s.toLowerCase();

/** Sections matching a query, with the matching entry ids. Empty query matches everything. */
export function searchSections(sections: readonly SettingsSection[], query: string): Array<{ section: SettingsSection; entryIds: string[] }> {
  const terms = norm(query).split(/\s+/).filter(Boolean);
  if (!terms.length) return sections.map((section) => ({ section, entryIds: [] }));
  const out: Array<{ section: SettingsSection; entryIds: string[] }> = [];
  for (const section of sections) {
    const sectionHay = norm([section.label, section.group ?? "", ...(section.keywords ?? [])].join(" "));
    const entryIds = (section.entries ?? []).filter((e) => terms.every((t) => norm([e.label, ...(e.keywords ?? [])].join(" ")).includes(t) || sectionHay.includes(t))).map((e) => e.id);
    const sectionHit = terms.every((t) => sectionHay.includes(t));
    if (sectionHit || entryIds.length) out.push({ section, entryIds: sectionHit && !entryIds.length ? [] : entryIds });
  }
  return out;
}

/** Scroll a row into view and flash it (search result, deep link). Resolves false when the row is not mounted. */
export function revealSetting(root: ParentNode, id: string): boolean {
  const el = root.querySelector<HTMLElement>(`[data-setting-id="${CSS.escape(id)}"]`);
  if (!el) return false;
  el.scrollIntoView({ block: "center" });
  el.classList.add("is-revealed");
  window.setTimeout(() => el.classList.remove("is-revealed"), 1600);
  return true;
}

/**
 * The active section's article: heading (`heading` ?? `label`) plus its body. With the heading hidden (`hideSectionHeading`
 * on the section or the panel) there is no `<h1>` and the article is labelled by `label`.
 */
export function SectionFrame({ section, ctx, hideSectionHeading = false }: { section: SettingsSection; ctx: SettingsContext; hideSectionHeading?: boolean }) {
  const body = <SettingsSectionScope id={section.id} title={section.label} keywords={sectionTerms(section)}>{section.render(ctx)}</SettingsSectionScope>;
  if ((section.hideSectionHeading ?? hideSectionHeading)) {
    return <article className="rk-settings__section" aria-label={section.label}>{body}</article>;
  }
  return (
    <article className="rk-settings__section" aria-labelledby={`rk-sec-${section.id}`}>
      <h1 id={`rk-sec-${section.id}`} className="rk-settings__h1">{section.heading ?? section.label}</h1>
      {body}
    </article>
  );
}

/** Terms that match every row of a section: its keywords and nav group. */
const sectionTerms = (s: SettingsSection): string[] | undefined => {
  const t = [...(s.keywords ?? []), ...(s.group ? [s.group] : [])];
  return t.length ? t : undefined;
};

/** State passed to `renderNavItem`. */
export interface NavItemState {
  active: boolean;
}

/** `navigate`: today's behaviour (the nav lists matching sections and entries, one section shows). `all-sections`: see `searchMode`. */
export type SearchMode = "navigate" | "all-sections";

export interface SettingsPanelProps {
  /**
   * Persistence port. Optional: without it the panel keeps an in-memory store, so sections that bring their own controls and
   * state need no backend. Sections still receive a `SettingsContext`.
   */
  backend?: SettingsBackend;
  sections: readonly SettingsSection[];
  title?: ReactNode;
  /** Nav heading. Takes precedence over `title`; `false` omits the heading. */
  navHeading?: ReactNode | false;
  /** Rendered at the bottom of the nav (e.g. a "Version 1.2.3" line). */
  navFooter?: ReactNode;
  /**
   * Replace the content of each nav button (icon, label, badge). The button itself stays, so keyboard navigation, `data-section-id`,
   * `aria-current="page"` and `data-active="true"` (active item only) keep working; draw your own active marker from `state.active`.
   */
  renderNavItem?: (section: SettingsSection, state: NavItemState) => ReactNode;
  /**
   * Omit the nav (the app embeds the sections in its own nav and selects with `activeId`). The search box is then left out too
   * unless `search` is set to `true` explicitly, in which case it sits above the content; a controlled `query` works either way.
   */
  hideNav?: boolean;
  /** Controlled active section id. */
  activeId?: string;
  onActiveChange?: (id: string) => void;
  defaultActiveId?: string;
  /** Same as `activeId`; wins when both are given. */
  activeSection?: string;
  /** Called alongside `onActiveChange`. */
  onActiveSectionChange?: (id: string) => void;
  /**
   * Show the search box. Default true with the nav; with `hideNav` it is shown only when this is `true` explicitly
   * (so an app that hides the nav today still gets no search box).
   */
  search?: boolean;
  /** Never render the built-in search box (a controlled `query` and your own field still search). Overrides `search`. */
  hideSearch?: boolean;
  /** Accessible name and placeholder of the search field. Default "Search settings". */
  searchLabel?: string;
  /** Controlled query. */
  query?: string;
  onQueryChange?: (query: string) => void;
  /**
   * `navigate` (default): the nav lists matching sections and entries. `all-sections`: while a query is active, every section's
   * matching rows (and `SettingsItem`s) render with their live controls under the section's title and an "Open section" action;
   * clearing the query restores the active section and the scroll position. Sections must use `SettingRow`/`SettingsItem` to take part.
   */
  searchMode?: SearchMode;
  /** Mark matching text in rows and group titles: `true` for `<mark>`, or a renderer. Default off. */
  highlight?: SettingsHighlight;
  /** Visible text of the "Open section" action (all-sections mode): a string, or `(title) => text`. Default "Open section". */
  openSectionLabel?: string | ((title: string) => string);
  /** Heading level of section titles in all-sections results. Default 2. */
  resultHeadingLevel?: 2 | 3 | 4 | 5 | 6;
  /** Announce the result count in a polite live region. Default: on in all-sections mode, off in navigate mode (so default markup is unchanged). */
  announceResults?: boolean;
  /** Text of the announcement. Default "N settings found" / "No settings found". */
  formatAnnouncement?: (count: number) => string;
  /** Cmd+F (Ctrl+F elsewhere) focuses the search field. A function decides for itself. Default off. */
  findShortcut?: boolean | ((e: globalThis.KeyboardEvent) => boolean);
  /** When Escape clears the query. Default: never. IME composition is respected. */
  escapeClears?: EscapeClears;
  /** Shown above the content while the backend loads. */
  loading?: ReactNode;
  /** Omit every section's `<h1>` (the nav still shows the label). A section's own `hideSectionHeading` overrides this. Default false. */
  hideSectionHeading?: boolean;
  /**
   * A snapshot already known to the caller (for example `createMemorySettings(...).snapshot()`). With it, the first render
   * shows the sections and never "Loading settings…".
   */
  initial?: SettingsSnapshot;
  className?: string;
}

/**
 * Settings frame: grouped section nav with search on the left, the active section on the right.
 * Persistence is the backend's; sections receive a `SettingsContext` and never touch storage themselves.
 */
export function SettingsPanel(props: SettingsPanelProps) {
  const { backend, sections, title = "Settings", navHeading, navFooter, renderNavItem, hideNav = false, defaultActiveId, search, hideSearch = false, searchLabel = "Search settings", loading, hideSectionHeading = false, initial, className } = props;
  const { searchMode = "navigate", highlight = false, openSectionLabel, resultHeadingLevel, findShortcut, escapeClears, formatAnnouncement } = props;
  const allSections = searchMode === "all-sections";
  const announce = props.announceResults ?? allSections;
  const fallback = useMemo(() => (backend ? undefined : createMemorySettings()), [backend]);
  const settings = useSettings(backend ?? fallback!, { initial: backend ? initial : (initial ?? fallback!.snapshot()) });
  const ctx: SettingsContext = settings;
  const ordered = useMemo(() => orderSections(sections.filter((s) => !s.visible || s.visible(ctx))), [sections, ctx]);
  const [innerQuery, setInnerQuery] = useState("");
  const query = props.query ?? innerQuery;
  const setQuery = (q: string) => {
    if (props.query === undefined) setInnerQuery(q);
    props.onQueryChange?.(q);
  };
  const [inner, setInner] = useState(defaultActiveId ?? "");
  const controlledActive = props.activeSection ?? props.activeId;
  const current = controlledActive ?? inner;
  const searching = allSections && query.trim() !== "";
  const matches = useMemo(() => (searching ? ordered.map((section) => ({ section, entryIds: [] as string[] })) : searchSections(ordered, query)), [ordered, query, searching]);
  const visibleIds = matches.map((m) => m.section.id);
  const active = ordered.find((s) => s.id === current && (!query || visibleIds.includes(s.id))) ?? matches[0]?.section;
  const content = useRef<HTMLDivElement>(null);
  const searchInput = useRef<HTMLInputElement>(null);
  const pendingReveal = useRef<string | null>(null);
  const showSearch = !hideSearch && (hideNav ? search === true : search !== false);

  const select = (id: string, reveal?: string) => {
    pendingReveal.current = reveal ?? null;
    if (controlledActive === undefined) setInner(id);
    props.onActiveChange?.(id);
    props.onActiveSectionChange?.(id);
  };

  // All-sections mode: remember the body's scroll when results take over, put it back when the query clears
  // (unless the user opened a section, which starts at the top).
  const savedScroll = useRef<number | null>(null);
  const skipRestore = useRef(false);
  useLayoutEffect(() => {
    const el = content.current;
    if (!el) return;
    if (searching && savedScroll.current === null) {
      savedScroll.current = el.scrollTop;
      el.scrollTop = 0;
    } else if (!searching && savedScroll.current !== null) {
      if (!skipRestore.current) el.scrollTop = savedScroll.current;
      savedScroll.current = null;
      skipRestore.current = false;
    }
  }, [searching]);

  const openSection = (id: string) => {
    skipRestore.current = true;
    setQuery("");
    select(id);
  };

  useSearchKeys({ inputRef: searchInput, query, setQuery, findShortcut, escapeClears });

  const searchField = showSearch ? (
    <input
      ref={searchInput}
      className="rk-input rk-settings__search"
      type="search"
      placeholder={searchLabel}
      aria-label={searchLabel}
      value={query}
      onChange={(e) => setQuery(e.target.value)}
    />
  ) : null;

  useEffect(() => {
    const id = pendingReveal.current;
    if (id && content.current) {
      pendingReveal.current = null;
      revealSetting(content.current, id);
    }
  });

  const onNavKey = (e: KeyboardEvent<HTMLElement>) => {
    if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
    const items = [...e.currentTarget.querySelectorAll<HTMLButtonElement>("button[data-section-id]")];
    const i = items.indexOf(document.activeElement as HTMLButtonElement);
    if (i === -1) return;
    e.preventDefault();
    items[(i + (e.key === "ArrowDown" ? 1 : -1) + items.length) % items.length]?.focus();
  };

  const heading = navHeading === undefined ? title : navHeading;
  let lastGroup: string | undefined;
  const navCount = matches.reduce((n, m) => n + (m.entryIds.length || 1), 0);
  return (
    <SettingsSearchProvider query={query} filter={searching} highlight={highlight}>
    <div className={cx("rk-settings", hideNav && "rk-settings--no-nav", className)} data-testid="settings-panel">
      {hideNav ? null : <nav className="rk-settings__nav" aria-label={typeof heading === "string" ? heading : typeof title === "string" ? title : "Settings"} onKeyDown={onNavKey}>
        {heading === false || heading === null ? null : <h2 className="rk-settings__title">{heading}</h2>}
        {searchField}
        {matches.length === 0 ? <p className="rk-settings__none">No settings match “{query}”.</p> : null}
        {matches.map(({ section, entryIds }) => {
          const heading = section.group !== lastGroup && section.group ? <div className="rk-settings__group">{section.group}</div> : null;
          lastGroup = section.group;
          return (
            <div key={section.id}>
              {heading}
              <button
                type="button"
                data-section-id={section.id}
                className={cx("rk-settings__item", active?.id === section.id && "is-active")}
                aria-current={active?.id === section.id ? "page" : undefined}
                data-active={active?.id === section.id ? "true" : undefined}
                onClick={() => (searching ? openSection(section.id) : select(section.id, entryIds[0]))}
              >
                {renderNavItem ? renderNavItem(section, { active: active?.id === section.id }) : (
                  <>
                    {section.icon ? <span className="rk-settings__icon" aria-hidden="true">{section.icon}</span> : null}
                    <span className="rk-settings__label">{section.label}</span>
                    {section.badge ? <span className="rk-settings__badge">{section.badge}</span> : null}
                  </>
                )}
              </button>
              {query
                ? (section.entries ?? [])
                    .filter((e) => entryIds.includes(e.id))
                    .map((e) => (
                      <button key={e.id} type="button" className="rk-settings__hit" onClick={() => select(section.id, e.id)}>
                        {e.label}
                      </button>
                    ))
                : null}
            </div>
          );
        })}
        {navFooter ? <div className="rk-settings__navfooter">{navFooter}</div> : null}
      </nav>}
      <div className="rk-settings__body" ref={content}>
        {hideNav && searchField ? <div className="rk-settings__searchbar" role="search">{searchField}</div> : null}
        {settings.error ? (
          <div className="rk-settings__error" role="alert">
            <span>{settings.error}</span>
            <button type="button" className="rk-btn rk-btn--sm rk-btn--ghost" onClick={settings.clearError}>
              Dismiss
            </button>
          </div>
        ) : null}
        {!settings.loaded ? (
          <div className="rk-settings__loading" aria-busy="true">{loading ?? "Loading settings…"}</div>
        ) : searching ? (
          <SearchResults sections={ordered} ctx={ctx} query={query} onOpen={openSection} openLabel={openSectionLabel} headingLevel={resultHeadingLevel} />
        ) : active ? (
          <SectionFrame key={active.id} section={active} ctx={ctx} hideSectionHeading={hideSectionHeading} />
        ) : null}
        <div className="rk-settings__saving" aria-live="polite">{settings.saving ? "Saving…" : ""}</div>
      </div>
      {announce ? <PanelAnnouncer searching={searching} activeQuery={query.trim() !== ""} navCount={navCount} format={formatAnnouncement} /> : null}
    </div>
    </SettingsSearchProvider>
  );
}

/** All-sections results: every section renders under its title; rows that do not match, and sections left empty, hide. */
function SearchResults({ sections, ctx, query, onOpen, openLabel, headingLevel }: { sections: readonly SettingsSection[]; ctx: SettingsContext; query: string; onOpen: (id: string) => void; openLabel?: string | ((title: string) => string); headingLevel?: 2 | 3 | 4 | 5 | 6 }) {
  const { count } = useSettingsSearch();
  return (
    <div className="rk-settings__results">
      {count === 0 ? <p className="rk-settings__none">No settings match “{query.trim()}”.</p> : null}
      {sections.map((section) => (
        <SettingsSearchSection key={section.id} id={section.id} title={section.label} keywords={sectionTerms(section)} onOpen={onOpen} openLabel={openLabel} headingLevel={headingLevel}>
          {section.render(ctx)}
        </SettingsSearchSection>
      ))}
    </div>
  );
}

function PanelAnnouncer({ searching, activeQuery, navCount, format }: { searching: boolean; activeQuery: boolean; navCount: number; format?: (count: number) => string }) {
  const { count } = useSettingsSearch();
  return <SettingsResultsAnnouncer count={searching ? count : navCount} active={activeQuery} format={format ?? defaultAnnouncement} />;
}
