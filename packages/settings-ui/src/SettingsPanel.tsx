import { useEffect, useMemo, useRef, useState, type KeyboardEvent, type ReactNode } from "react";
import { cx } from "./primitives.js";
import type { SettingsBackend, SettingsContext, SettingsSection } from "./types.js";
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

/** The active section's article: heading (`heading` ?? `label`) plus its body. */
export function SectionFrame({ section, ctx }: { section: SettingsSection; ctx: SettingsContext }) {
  return (
    <article className="rk-settings__section" aria-labelledby={`rk-sec-${section.id}`}>
      <h1 id={`rk-sec-${section.id}`} className="rk-settings__h1">{section.heading ?? section.label}</h1>
      {section.render(ctx)}
    </article>
  );
}

export interface SettingsPanelProps {
  backend: SettingsBackend;
  sections: readonly SettingsSection[];
  title?: ReactNode;
  /** Controlled active section id. */
  activeId?: string;
  onActiveChange?: (id: string) => void;
  defaultActiveId?: string;
  /** Show the search box. Default true. */
  search?: boolean;
  /** Shown above the content while the backend loads. */
  loading?: ReactNode;
  className?: string;
}

/**
 * Settings frame: grouped section nav with search on the left, the active section on the right.
 * Persistence is the backend's; sections receive a `SettingsContext` and never touch storage themselves.
 */
export function SettingsPanel({ backend, sections, title = "Settings", activeId, onActiveChange, defaultActiveId, search = true, loading, className }: SettingsPanelProps) {
  const settings = useSettings(backend);
  const ctx: SettingsContext = settings;
  const ordered = useMemo(() => orderSections(sections.filter((s) => !s.visible || s.visible(ctx))), [sections, ctx]);
  const [query, setQuery] = useState("");
  const [inner, setInner] = useState(defaultActiveId ?? "");
  const current = activeId ?? inner;
  const matches = useMemo(() => searchSections(ordered, query), [ordered, query]);
  const visibleIds = matches.map((m) => m.section.id);
  const active = ordered.find((s) => s.id === current && (!query || visibleIds.includes(s.id))) ?? matches[0]?.section;
  const content = useRef<HTMLDivElement>(null);
  const pendingReveal = useRef<string | null>(null);

  const select = (id: string, reveal?: string) => {
    pendingReveal.current = reveal ?? null;
    if (activeId === undefined) setInner(id);
    onActiveChange?.(id);
  };

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

  let lastGroup: string | undefined;
  return (
    <div className={cx("rk-settings", className)} data-testid="settings-panel">
      <nav className="rk-settings__nav" aria-label={typeof title === "string" ? title : "Settings"} onKeyDown={onNavKey}>
        <h2 className="rk-settings__title">{title}</h2>
        {search ? (
          <input
            className="rk-input rk-settings__search"
            type="search"
            placeholder="Search settings"
            aria-label="Search settings"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
          />
        ) : null}
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
                onClick={() => select(section.id, entryIds[0])}
              >
                {section.icon ? <span className="rk-settings__icon" aria-hidden="true">{section.icon}</span> : null}
                <span className="rk-settings__label">{section.label}</span>
                {section.badge ? <span className="rk-settings__badge">{section.badge}</span> : null}
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
      </nav>
      <div className="rk-settings__body" ref={content}>
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
        ) : active ? (
          <SectionFrame key={active.id} section={active} ctx={ctx} />
        ) : null}
        <div className="rk-settings__saving" aria-live="polite">{settings.saving ? "Saving…" : ""}</div>
      </div>
    </div>
  );
}
