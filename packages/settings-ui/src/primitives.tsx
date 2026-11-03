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
  /** Added after `rk-sg__title` on the heading (0.4.4). */
  titleClassName?: string;
  /** Added after `rk-sg__desc` on the description, the group's hint (0.4.4). */
  hintClassName?: string;
  /** Added after `rk-sg__rows` on the rows container (0.4.4). */
  rowsClassName?: string;
}

/** Titled group of rows inside a section. */
export function SettingsGroup({ title, description, children, hideWhenEmpty = false, className, headingLevel = 3, titleClassName, hintClassName, rowsClassName }: SettingsGroupProps) {
  const g = useGroupScope(title, description, hideWhenEmpty);
  if (g.unmounted) return <GroupScope groupKey={g.key} title={g.titleText}>{children}</GroupScope>;
  return (
    <section className={cx("rk-sg", className)} style={g.hidden ? { display: "none" } : undefined}>
      {title ? createElement(`h${headingLevel}`, { className: cx("rk-sg__title", titleClassName) }, typeof title === "string" ? g.mark(title) : title) : null}
      {description ? <p className={cx("rk-sg__desc", hintClassName)}>{typeof description === "string" ? g.mark(description) : description}</p> : null}
      <div className={cx("rk-sg__rows", rowsClassName)}><GroupScope groupKey={g.key} title={g.titleText}>{children}</GroupScope></div>
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
  /** Rendered before the text, for an icon or badge (0.4.4). Adds `rk-sr--lead` and the `rk-sr__lead` wrapper. */
  lead?: ReactNode;
  /** Added after `rk-sr__label` on the title (0.4.4). */
  titleClassName?: string;
  /** Added after `rk-sr__desc` on the hint (0.4.4). */
  hintClassName?: string;
  /**
   * Render `children` directly, without the `rk-sr__control` wrapper (0.4.4). Each child becomes a grid item of the row, so pass
   * one element. Default false.
   */
  bareControl?: boolean;
  /**
   * Pro-gated row: the control is disabled (its controls sit in a disabled `fieldset`) and the label carries `lockedLabel`.
   * Default false.
   */
  locked?: boolean;
  /** Badge text for a `locked` row. Default "Pro". */
  lockedLabel?: ReactNode;
}

export function SettingRow({ id, label, description, children, stacked, keywords, searchText, className, lead, titleClassName, hintClassName, bareControl = false, locked = false, lockedLabel }: SettingRowProps) {
  const { visible, mark } = useRowSearch({ id, label, description, keywords, searchText });
  if (!visible) return null;
  const hasLead = lead !== undefined && lead !== null && lead !== false;
  const control = locked ? <fieldset className="rk-sr__lock-fs" disabled>{children}</fieldset> : children;
  return (
    <div className={cx("rk-sr", stacked && "rk-sr--stacked", hasLead && "rk-sr--lead", locked && "rk-sr--locked", className)} data-setting-id={id} data-locked={locked ? "true" : undefined}>
      {hasLead ? <div className="rk-sr__lead">{lead}</div> : null}
      <div className="rk-sr__text">
        <div className={cx("rk-sr__label", titleClassName)}>
          {mark(label)}
          {locked ? <> <span className="rk-sr__lock">{lockedLabel ?? "Pro"}</span></> : null}
        </div>
        {description ? <div className={cx("rk-sr__desc", hintClassName)}>{mark(description)}</div> : null}
      </div>
      {children ? (bareControl ? control : <div className="rk-sr__control">{control}</div>) : null}
    </div>
  );
}

/** Native select for a settings row (value and options are the app's). */
export interface SelectControlProps {
  id?: string;
  /** Accessible name of the select. */
  label: string;
  value: string;
  options: ReadonlyArray<{ value: string; label: string; disabled?: boolean }>;
  onChange: (value: string) => void;
  disabled?: boolean;
}

export function SelectControl({ id, label, value, options, onChange, disabled = false }: SelectControlProps) {
  return (
    <select id={id} className="rk-input rk-settings__select" aria-label={label} value={value} disabled={disabled} onChange={(e) => onChange(e.target.value)}>
      {options.map((o) => (
        <option key={o.value} value={o.value} disabled={o.disabled}>
          {o.label}
        </option>
      ))}
    </select>
  );
}

/**
 * Path picker for a settings row: shows the chosen path (read-only) and a button that calls `onPick` (the app opens its own
 * file or folder dialog). `onClear`, when given, adds a Clear button.
 */
export interface PathControlProps {
  id?: string;
  /** Accessible name of the path display. */
  label: string;
  value: string | null;
  /** Shown while no path is chosen. Default "Not set". */
  placeholder?: string;
  /** Default "Choose…". */
  pickLabel?: string;
  onPick: () => void | Promise<void>;
  onClear?: () => void;
  disabled?: boolean;
}

export function PathControl({ id, label, value, placeholder = "Not set", pickLabel = "Choose…", onPick, onClear, disabled = false }: PathControlProps) {
  return (
    <div className="rk-sr__inline rk-path">
      <output id={id} className="rk-path__value" aria-label={label}>{value || placeholder}</output>
      <button type="button" className="rk-btn rk-btn--sm rk-btn--secondary" disabled={disabled} onClick={() => void onPick()}>
        {pickLabel}
      </button>
      {onClear && value ? (
        <button type="button" className="rk-btn rk-btn--sm rk-btn--ghost" disabled={disabled} onClick={onClear}>
          Clear
        </button>
      ) : null}
    </div>
  );
}

/** Index progress line: a progress bar and count while indexing, or the idle sentence otherwise. */
export interface IndexProgressLineProps {
  done: number;
  total: number;
  running: boolean;
  /** Shown when not running. Default "Index is up to date." */
  idle?: ReactNode;
  /** Prefix of the running line. Default "Indexing". */
  runningLabel?: string;
}

export function IndexProgressLine({ done, total, running, idle = "Index is up to date.", runningLabel = "Indexing" }: IndexProgressLineProps) {
  if (!running) return <span role="status">{idle}</span>;
  const pct = total > 0 ? Math.min(100, Math.round((done / total) * 100)) : 0;
  return (
    <span role="status" className="rk-index-progress">
      {`${runningLabel} ${done.toLocaleString()} of ${total.toLocaleString()} (${pct}%)`}
      <ProgressBar value={pct / 100} label={`${runningLabel} progress`} />
    </span>
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
