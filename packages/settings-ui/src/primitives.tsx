import type { ReactNode } from "react";

export const cx = (...parts: Array<string | false | null | undefined>) => parts.filter(Boolean).join(" ");

/** Titled group of rows inside a section. */
export function SettingsGroup({ title, description, children }: { title?: ReactNode; description?: ReactNode; children: ReactNode }) {
  return (
    <section className="rk-sg">
      {title ? <h3 className="rk-sg__title">{title}</h3> : null}
      {description ? <p className="rk-sg__desc">{description}</p> : null}
      <div className="rk-sg__rows">{children}</div>
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
}

export function SettingRow({ id, label, description, children, stacked }: SettingRowProps) {
  return (
    <div className={cx("rk-sr", stacked && "rk-sr--stacked")} data-setting-id={id}>
      <div className="rk-sr__text">
        <div className="rk-sr__label">{label}</div>
        {description ? <div className="rk-sr__desc">{description}</div> : null}
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
