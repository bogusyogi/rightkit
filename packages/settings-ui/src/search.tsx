import { createElement, useContext, useId, useMemo, type ReactNode } from "react";
import {
  ConfigContext,
  SectionContext,
  highlightText,
  useSearchEntry,
  useSettingsSearchStatus,
  useVisibleCount,
  type SettingsSectionScopeProps,
} from "./search-core.js";

/*
 * The styled search parts. The context, provider, hooks and keyboard logic live in `search-core.tsx` (no markup, no stylesheet);
 * everything exported from there is re-exported here so existing imports keep working.
 */
export * from "./search-core.js";

/** A search result item. Renders one `<div>` only while it matches (or always, outside a search). */
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
  /**
   * Opt-in (0.4.2). While no search is active, render the children without this component's own section, heading and action: the
   * section's rows keep their section context and the page looks as it did before search. Default false (the section always renders).
   */
  passthroughWhenInactive?: boolean;
  className?: string;
}

/** One section's block in a results list: a heading, an optional "Open section" action, and the section's own content. */
export function SettingsSearchSection({ id, title, keywords, onOpen, openLabel = "Open section", headingLevel = 2, hideWhenEmpty = true, passthroughWhenInactive = false, className, children }: SettingsSearchSectionProps) {
  const cfg = useContext(ConfigContext);
  const key = useId();
  const headingId = `rk-search-sec-${id}`;
  const value = useMemo(() => ({ key, id, title, keywords }), [key, id, title, keywords]);
  const visibleRows = useVisibleCount((e) => e.scopeKey === key);
  const hidden = hideWhenEmpty && !!cfg && visibleRows === 0;
  if (passthroughWhenInactive && !(cfg && cfg.terms.length > 0)) {
    return <SectionContext.Provider value={value}>{children}</SectionContext.Provider>;
  }
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

/**
 * Polite live region carrying the result count. Its text updates `delay` ms after the count settles, so typing does not
 * produce one announcement per keystroke. Empty when there is no query.
 */
export function SettingsResultsAnnouncer({ count, active, format, delay = 300, className = "rk-settings__sr" }: { count: number; active: boolean; format?: (count: number) => string; delay?: number; className?: string }) {
  const shown = useSettingsSearchStatus({ count, active, format, delay });
  return <div className={className} role="status" aria-live="polite" aria-atomic="true">{shown}</div>;
}
