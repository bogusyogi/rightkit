import { createElement, type ReactNode } from "react";
import { GroupScope, useGroupScope, useRowSearch } from "./search.js";

export const cx = (...parts: Array<string | false | null | undefined>) => parts.filter(Boolean).join(" ");

export interface SettingsGroupProps {
  title?: ReactNode;
  description?: ReactNode;
  children: ReactNode;
  /**
   * Under a `SettingsSearchProvider`: hide the group while none of its registered rows or items is visible (every row is
   * filtered out, or none rendered because the backend does not publish its key). Re-evaluated whenever registrations change.
   * Content that is not a `SettingRow` must be wrapped in `SettingsItem` to count. Default false.
   *
   * `true` keeps the section in the DOM with `display: none`. `"unmount"` (0.4.2) renders no section and no heading while the
   * group is empty; the rows stay mounted (they render nothing while they do not match), so a later match brings the group back.
   * Rows that remount when the group empties or refills lose their own local state, so keep drafts in state the app owns.
   */
  hideWhenEmpty?: boolean | "unmount";
  /** Added after the `rk-sg` class on the `<section>` (0.4.2). */
  className?: string;
  /** Level of the title heading (0.4.2). Default 3. */
  headingLevel?: 2 | 3 | 4 | 5 | 6;
}

/** Titled group of rows inside a section. */
export function SettingsGroup({ title, description, children, hideWhenEmpty = false, className, headingLevel = 3 }: SettingsGroupProps) {
  const g = useGroupScope(title, description, hideWhenEmpty);
  if (g.unmounted) return <GroupScope groupKey={g.key} title={g.titleText}>{children}</GroupScope>;
  return (
    <section className={cx("rk-sg", className)} style={g.hidden ? { display: "none" } : undefined}>
      {title ? createElement(`h${headingLevel}`, { className: "rk-sg__title" }, typeof title === "string" ? g.mark(title) : title) : null}
      {description ? <p className="rk-sg__desc">{typeof description === "string" ? g.mark(description) : description}</p> : null}
      <div className="rk-sg__rows"><GroupScope groupKey={g.key} title={g.titleText}>{children}</GroupScope></div>
    </section>
  );
}

export interface SettingRowProps {
  /** Becomes `data-setting-id`; keep it equal to the section entry id so search can reveal the row. */
  id?: string;
  label: ReactNode;
  description?: ReactNode;
  /** The control, right-aligned. */
  children?: ReactNode;
  /** Put the control under the text (wide controls: inputs, lists). */
  stacked?: boolean;
  /** Extra search terms (under a `SettingsSearchProvider`). */
  keywords?: readonly string[];
  /** What search matches when `label` is not a string. */
  searchText?: string;
  /** Added after the row's own classes (0.4.2). */
  className?: string;
}

export function SettingRow({ id, label, description, children, stacked, keywords, searchText, className }: SettingRowProps) {
  const { visible, mark } = useRowSearch({ id, label, description, keywords, searchText });
  if (!visible) return null;
  return (
    <div className={cx("rk-sr", stacked && "rk-sr--stacked", className)} data-setting-id={id}>
      <div className="rk-sr__text">
        <div className="rk-sr__label">{mark(label)}</div>
        {description ? <div className="rk-sr__desc">{mark(description)}</div> : null}
      </div>
      {children ? <div className="rk-sr__control">{children}</div> : null}
    </div>
  );
}

export function ProgressBar({ value, label }: { value: number; label: string }) {
  const pct = Math.max(0, Math.min(1, value));
  return (
    <div className="rk-sprog" role="progressbar" aria-label={label} aria-valuemin={0} aria-valuemax={100} aria-valuenow={Math.round(pct * 100)}>
      <i style={{ width: `${pct * 100}%` }} />
    </div>
  );
}

export function formatBytes(n: number | undefined): string {
  if (n === undefined) return "";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let v = n;
  let u = 0;
  while (v >= 1000 && u < units.length - 1) {
    v /= 1000;
    u += 1;
  }
  return `${v >= 100 || u === 0 ? Math.round(v) : v.toFixed(1)} ${units[u]}`;
}
