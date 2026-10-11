import {
  createContext,
  createElement,
  useCallback,
  useContext,
  useEffect,
  useId,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  useSyncExternalStore,
  type ReactNode,
  type RefObject,
} from "react";

/*
 * Search context. Rows, groups and items register on mount and unregister on unmount, so what is searchable is exactly what
 * is rendered: a conditional row that is not mounted is never a result, and a row that loads late is a result as soon as it mounts.
 * Everything here is inert without a `SettingsSearchProvider` (no provider: nothing registers, nothing hides, nothing is marked).
 */

const useIsoLayoutEffect = typeof window === "undefined" ? useEffect : useLayoutEffect;

export type SettingsSearchKind = "row" | "item" | "group";

/** What a row, group or item contributes to search. */
export interface SettingsSearchEntry {
  kind: SettingsSearchKind;
  label: string;
  description?: string;
  keywords?: readonly string[];
  /** Title of the enclosing `SettingsGroup`, when it is a string. */
  group?: string;
  /** Title of the enclosing section (`SettingsSectionScope` / `SettingsSearchSection`). */
  section?: string;
  sectionId?: string;
  /** Extra section search terms (the section's own keywords and nav group). */
  sectionKeywords?: readonly string[];
  /** Internal grouping keys. */
  groupKey?: string;
  scopeKey?: string;
  /** `data-setting-id` of the row or item, when given. */
  id?: string;
}

/** A registered row or item that matches the current query. */
export type SettingsSearchResult = Omit<SettingsSearchEntry, "kind" | "groupKey" | "scopeKey" | "sectionKeywords"> & { kind: "row" | "item" };

/** `true` wraps matches in `<mark class="rk-settings__mark">`; a function renders each matched piece itself. Default: no highlight. */
export type SettingsHighlight = boolean | ((match: string) => ReactNode);

interface Stored extends SettingsSearchEntry {
  hay: string;
}

const textOf = (n: ReactNode): string => (typeof n === "string" || typeof n === "number" ? String(n) : "");
const hayOf = (e: SettingsSearchEntry) => [e.label, e.description, ...(e.keywords ?? []), e.group, e.section, ...(e.sectionKeywords ?? [])].filter(Boolean).join(" ").toLowerCase();

/** Lower-cased whitespace-separated terms; every term must match somewhere in a row's text. */
export function parseSearchTerms(query: string): string[] {
  return query.toLowerCase().split(/\s+/).filter(Boolean);
}
const matchesAll = (terms: readonly string[], hay: string) => terms.every((t) => hay.includes(t));

interface Store {
  register(entry: SettingsSearchEntry): () => void;
  subscribe(l: () => void): () => void;
  version(): number;
  all(): Stored[];
}

function createStore(): Store {
  const entries = new Map<number, Stored>();
  const listeners = new Set<() => void>();
  let n = 0;
  let version = 0;
  const bump = () => {
    version += 1;
    for (const l of [...listeners]) l();
  };
  return {
    register(entry) {
      const key = (n += 1);
      entries.set(key, { ...entry, hay: hayOf(entry) });
      bump();
      return () => {
        if (entries.delete(key)) bump();
      };
    },
    subscribe(l) {
      listeners.add(l);
      return () => void listeners.delete(l);
    },
    version: () => version,
    all: () => [...entries.values()],
  };
}
const NULL_STORE = createStore();

interface Config {
  store: Store;
  query: string;
  terms: readonly string[];
  filter: boolean;
  highlight: SettingsHighlight;
}
const ConfigContext = createContext<Config | null>(null);
const SectionContext = createContext<{ key: string; id: string; title: string; keywords?: readonly string[] } | null>(null);
const GroupContext = createContext<{ key: string; title?: string } | null>(null);

/** An entry counts as a hit when no query is active, when filtering is off, or when every term matches. */
const isVisible = (cfg: Config, e: Stored) => cfg.terms.length === 0 || !cfg.filter || matchesAll(cfg.terms, e.hay);

/** Wrap each occurrence of a term in `text`. Returns `text` untouched when nothing matches or highlighting is off. */
export function highlightText(text: string, terms: readonly string[], highlight: SettingsHighlight): ReactNode {
  if (!highlight || terms.length === 0 || !text) return text;
  const lower = text.toLowerCase();
  if (lower.length !== text.length) return text; // case folding changed the length; offsets would drift
  const ranges: Array<[number, number]> = [];
  for (const t of terms) {
    for (let at = lower.indexOf(t); at !== -1; at = lower.indexOf(t, at + t.length)) ranges.push([at, at + t.length]);
  }
  if (!ranges.length) return text;
  ranges.sort((a, b) => a[0] - b[0] || b[1] - a[1]);
  const merged: Array<[number, number]> = [];
  for (const r of ranges) {
    const last = merged[merged.length - 1];
    if (last && r[0] <= last[1]) last[1] = Math.max(last[1], r[1]);
    else merged.push([r[0], r[1]]);
  }
  const out: ReactNode[] = [];
  let pos = 0;
  merged.forEach(([s, e], i) => {
    if (s > pos) out.push(text.slice(pos, s));
    const piece = text.slice(s, e);
    out.push(typeof highlight === "function" ? <span key={i} style={{ display: "contents" }}>{highlight(piece)}</span> : <mark key={i} className="rk-settings__mark">{piece}</mark>);
    pos = e;
  });
  if (pos < text.length) out.push(text.slice(pos));
  return <>{out}</>;
}

export interface SettingsSearchProviderProps {
  /** The current query (trimmed or not; split on whitespace, every term must match). Empty means no search. */
  query: string;
  /**
   * Hide registered rows and items that do not match (default true). With `false` the query still drives highlight and `results`
   * but nothing is hidden (the panel's "navigate" mode).
   */
  filter?: boolean;
  /** Mark the matching text. Default off. */
  highlight?: SettingsHighlight;
  children?: ReactNode;
}

/** Provides the query to every row, group and item below it and collects their registrations. Renders no DOM. */
export function SettingsSearchProvider({ query, filter = true, highlight = false, children }: SettingsSearchProviderProps) {
  const store = useRef<Store | null>(null);
  store.current ??= createStore();
  const terms = useMemo(() => parseSearchTerms(query), [query]);
  const value = useMemo<Config>(() => ({ store: store.current!, query, terms, filter, highlight }), [query, terms, filter, highlight]);
  return <ConfigContext.Provider value={value}>{children}</ConfigContext.Provider>;
}

export interface SettingsSearchState {
  query: string;
  terms: readonly string[];
  /** A query with at least one term is in effect. */
  active: boolean;
  /** Registered rows and items matching the query (every registered one when there is no query). */
  results: SettingsSearchResult[];
  count: number;
  /** Register an entry; returns the unregister function. Rows, groups and items call this themselves. */
  register(entry: SettingsSearchEntry): () => void;
  /** Wrap the query terms in `text` as the provider's `highlight` option says. */
  highlight(text: string): ReactNode;
}

const INACTIVE: SettingsSearchState = {
  query: "",
  terms: [],
  active: false,
  results: [],
  count: 0,
  register: () => () => {},
  highlight: (t) => t,
};

/** The search state below a `SettingsSearchProvider`; re-renders when registrations change. Outside a provider it is inert. */
export function useSettingsSearch(): SettingsSearchState {
  const cfg = useContext(ConfigContext);
  const store = cfg?.store ?? NULL_STORE;
  const version = useSyncExternalStore(store.subscribe, store.version, store.version);
  return useMemo(() => {
    if (!cfg) return INACTIVE;
    const results = store
      .all()
      .filter((e) => e.kind !== "group" && (cfg.terms.length === 0 || matchesAll(cfg.terms, e.hay)))
      .map(({ hay: _hay, groupKey: _g, scopeKey: _s, sectionKeywords: _k, kind, ...rest }) => ({ ...rest, kind: kind as "row" | "item" }));
    return {
      query: cfg.query,
      terms: cfg.terms,
      active: cfg.terms.length > 0,
      results,
      count: results.length,
      register: store.register,
      highlight: (text) => highlightText(text, cfg.terms, cfg.highlight),
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [cfg, store, version]);
}

/** `text` with the query marked, for content inside a `SettingsItem`. Plain text outside a provider or with highlighting off. */
export function HighlightedText({ text }: { text: string }) {
  const cfg = useContext(ConfigContext);
  return <>{cfg ? highlightText(text, cfg.terms, cfg.highlight) : text}</>;
}

function useVisibleCount(pred: (e: Stored) => boolean): number {
  const cfg = useContext(ConfigContext);
  const store = cfg?.store ?? NULL_STORE;
  useSyncExternalStore(store.subscribe, store.version, store.version);
  if (!cfg) return 0;
  return store.all().filter((e) => e.kind !== "group" && pred(e) && isVisible(cfg, e)).length;
}

interface EntryInput {
  kind: SettingsSearchKind;
  label?: ReactNode;
  description?: ReactNode;
  keywords?: readonly string[];
  searchText?: string;
  id?: string;
}

/** Registers one entry for its mount and reports whether it is a visible result. */
function useSearchEntry(p: EntryInput) {
  const cfg = useContext(ConfigContext);
  const section = useContext(SectionContext);
  const group = useContext(GroupContext);
  const label = p.searchText ?? textOf(p.label);
  const description = textOf(p.description);
  const kw = p.keywords?.join("\u0001") ?? "";
  const secKw = section?.keywords?.join("\u0001") ?? "";
  const store = cfg?.store;
  useIsoLayoutEffect(() => {
    if (!store) return;
    return store.register({
      kind: p.kind,
      label,
      description: description || undefined,
      keywords: p.keywords,
      group: group?.title,
      groupKey: group?.key,
      section: section?.title,
      sectionId: section?.id,
      sectionKeywords: section?.keywords,
      scopeKey: section?.key,
      id: p.id,
    });
    // The joined strings stand in for the arrays so an inline array literal does not re-register on every render.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [store, p.kind, p.id, label, description, kw, secKw, group?.title, group?.key, section?.title, section?.id, section?.key]);
  const terms = cfg?.terms ?? [];
  let visible = true;
  if (cfg && cfg.filter && terms.length) {
    visible = matchesAll(terms, hayOf({ kind: p.kind, label, description, keywords: p.keywords, group: group?.title, section: section?.title, sectionKeywords: section?.keywords }));
  }
  const mark = (text: ReactNode): ReactNode => (cfg && typeof text === "string" ? highlightText(text, cfg.terms, cfg.highlight) : text);
  return { visible, mark };
}

/** Group scope used by `SettingsGroup`. */
export function useGroupScope(title: ReactNode, description: ReactNode, hideWhenEmpty: boolean) {
  const cfg = useContext(ConfigContext);
  const key = useId();
  const own = useSearchEntry({ kind: "group", label: title, description });
  const visibleRows = useVisibleCount((e) => e.groupKey === key);
  const hidden = hideWhenEmpty && !!cfg && visibleRows === 0;
  return { key, titleText: typeof title === "string" ? title : undefined, mark: own.mark, hidden };
}

/** Provides the group's key and title to the rows inside it. Renders no DOM. */
export function GroupScope({ groupKey, title, children }: { groupKey: string; title?: string; children?: ReactNode }) {
  const value = useMemo(() => ({ key: groupKey, title }), [groupKey, title]);
  return <GroupContext.Provider value={value}>{children}</GroupContext.Provider>;
}

/** Row hook used by `SettingRow`. */
export function useRowSearch(p: { label: ReactNode; description?: ReactNode; keywords?: readonly string[]; searchText?: string; id?: string }) {
  return useSearchEntry({ kind: "row", ...p });
}

export interface SettingsItemProps {
  /** What search matches. */
  text: string;
  description?: string;
  keywords?: readonly string[];
  /** `data-setting-id` (so `revealSetting` can find it). */
  id?: string;
  className?: string;
  children?: ReactNode;
}

/**
 * Lets content that is not a `SettingRow` (an account list, a permission row, a launcher list) take part in search with the
 * text you give it. Shown only while it matches; counted as a result for its group and section. Renders one `<div>`.
 */
export function SettingsItem({ text, description, keywords, id, className, children }: SettingsItemProps) {
  const { visible } = useSearchEntry({ kind: "item", label: text, description, keywords, id });
  if (!visible) return null;
  return <div className={className} data-setting-id={id}>{children}</div>;
}

export interface SettingsSectionScopeProps {
  id: string;
  title: string;
  /** Extra terms that match every row in the section (the section's keywords and nav group). */
  keywords?: readonly string[];
  children?: ReactNode;
}

/** Tells rows below it which section they belong to (title and id are part of what they match and report). Renders no DOM. */
export function SettingsSectionScope({ id, title, keywords, children }: SettingsSectionScopeProps) {
  const key = useId();
  const value = useMemo(() => ({ key, id, title, keywords }), [key, id, title, keywords]);
  return <SectionContext.Provider value={value}>{children}</SectionContext.Provider>;
}

export interface SettingsSearchSectionProps extends SettingsSectionScopeProps {
  /** Renders the "Open section" action when given. */
  onOpen?: (id: string) => void;
  /**
   * Visible text of the action. A string (default "Open section") gets the section title added to its accessible name; a function
   * (`(title) => "Open " + title`) supplies the whole text, which is then the name.
   */
  openLabel?: string | ((title: string) => string);
  /** Heading level of the section title (a real heading). Default 2. */
  headingLevel?: 2 | 3 | 4 | 5 | 6;
  /** Hide the whole section while none of its registered rows or items is visible. Default true. */
  hideWhenEmpty?: boolean;
  className?: string;
}

/** One section's block in a results list: a heading, an optional "Open section" action, and the section's own content. */
export function SettingsSearchSection({ id, title, keywords, onOpen, openLabel = "Open section", headingLevel = 2, hideWhenEmpty = true, className, children }: SettingsSearchSectionProps) {
  const cfg = useContext(ConfigContext);
  const key = useId();
  const headingId = `rk-search-sec-${id}`;
  const value = useMemo(() => ({ key, id, title, keywords }), [key, id, title, keywords]);
  const visibleRows = useVisibleCount((e) => e.scopeKey === key);
  const hidden = hideWhenEmpty && !!cfg && visibleRows === 0;
  const heading = createElement(`h${headingLevel}`, { id: headingId, className: "rk-settings__result-title" }, cfg ? highlightText(title, cfg.terms, cfg.highlight) : title);
  return (
    <SectionContext.Provider value={value}>
      <section className={["rk-settings__result", className].filter(Boolean).join(" ")} aria-labelledby={headingId} data-section-id={id} style={hidden ? { display: "none" } : undefined}>
        <div className="rk-settings__result-head">
          {heading}
          {onOpen ? (
            <button type="button" className="rk-btn rk-btn--sm rk-btn--ghost" aria-label={typeof openLabel === "string" ? `${openLabel} ${title}` : undefined} onClick={() => onOpen(id)}>
              {typeof openLabel === "string" ? openLabel : openLabel(title)}
            </button>
          ) : null}
        </div>
        {children}
      </section>
    </SectionContext.Provider>
  );
}

export const defaultAnnouncement = (count: number) => (count === 0 ? "No settings found" : count === 1 ? "1 setting found" : `${count} settings found`);

/**
 * Polite live region carrying the result count. Its text updates `delay` ms after the count settles, so typing does not
 * produce one announcement per keystroke. Empty when there is no query.
 */
export function SettingsResultsAnnouncer({ count, active, format = defaultAnnouncement, delay = 300, className = "rk-settings__sr" }: { count: number; active: boolean; format?: (count: number) => string; delay?: number; className?: string }) {
  const text = active ? format(count) : "";
  const shown = useDebounced(text, text === "" ? 0 : delay);
  return <div className={className} role="status" aria-live="polite" aria-atomic="true">{shown}</div>;
}

function useDebounced(value: string, delay: number): string {
  const [shown, setShown] = useState(value);
  useEffect(() => {
    if (shown === value) return;
    if (delay === 0) {
      setShown(value);
      return;
    }
    const t = setTimeout(() => setShown(value), delay);
    return () => clearTimeout(t);
  }, [value, delay, shown]);
  return shown;
}

/* ---- keyboard ---- */

/** When Escape clears the query. Omit for never (the default). */
export type EscapeClears = "always" | "when-search-focused" | "unless-in-input-or-dialog";

export interface SearchKeyOptions {
  /** The search field Cmd/Ctrl+F focuses and Escape is judged against. */
  inputRef: RefObject<HTMLInputElement | null>;
  query: string;
  setQuery(q: string): void;
  /** Cmd+F (Ctrl+F elsewhere) focuses and selects the search field. A function decides for itself. Default off. */
  findShortcut?: boolean | ((e: KeyboardEvent) => boolean);
  escapeClears?: EscapeClears;
}

const isMac = () => typeof navigator !== "undefined" && /Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent || "");
const composing = (e: KeyboardEvent) => e.isComposing || e.keyCode === 229;
const OTHER_FIELD = "input, textarea, select, [contenteditable]:not([contenteditable='false']), [role='dialog'], dialog";

/** The find shortcut and Escape policy as a hook, for hosts that own their search field. The panel uses it for its own. */
export function useSearchKeys(opts: SearchKeyOptions): void {
  const ref = useRef(opts);
  ref.current = opts;
  const on = !!opts.findShortcut || !!opts.escapeClears;
  useEffect(() => {
    if (!on) return;
    const onKey = (e: KeyboardEvent) => {
      const o = ref.current;
      if (composing(e)) return;
      const find = typeof o.findShortcut === "function" ? o.findShortcut(e) : o.findShortcut && (isMac() ? e.metaKey && !e.ctrlKey : e.ctrlKey && !e.metaKey) && !e.altKey && !e.shiftKey && e.key.toLowerCase() === "f";
      if (find) {
        const input = o.inputRef.current;
        if (!input) return;
        e.preventDefault();
        input.focus();
        input.select();
        return;
      }
      if (e.key !== "Escape" || !o.escapeClears || e.defaultPrevented || o.query === "") return;
      const input = o.inputRef.current;
      const target = e.target as Element | null;
      const onInput = !!input && target === input;
      let clear = false;
      if (o.escapeClears === "always") clear = true;
      else if (o.escapeClears === "when-search-focused") clear = onInput;
      else clear = onInput || !(target instanceof Element && target.closest(OTHER_FIELD));
      if (clear) o.setQuery("");
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [on]);
}
