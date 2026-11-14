import { cx } from "../lib/ui.js";
import { formatChord } from "./chord.js";
import type { OS, PermissionState } from "./types.js";

function ChordKeysActive({
  chord,
  os,
  activeKeys,
  active = false,
}: {
  chord: string;
  os: OS;
  activeKeys: string[];
  active?: boolean;
}) {
  const displayActiveKeys = active ? formatChord(chord, os) : activeKeys;
  return (
    <span className="inline-flex flex-wrap items-center gap-2 align-middle">
      {formatChord(chord, os).map((key) => (
        <kbd
          key={key}
          className={cx(
            "inline-grid min-h-[24px] min-w-[30px] place-items-center rounded-[var(--radius-sm)] border px-2.5 [font-family:var(--font-mono)] text-[11px] font-semibold leading-none shadow-[0_1px_0_var(--line)] transition-[background-color,border-color,box-shadow,transform] duration-100",
            displayActiveKeys.includes(key)
              ? "scale-[1.05] border-[var(--accent)] border-b-[var(--accent-hover)] bg-[var(--tint-accent)] text-[var(--paper)] shadow-[0_0_0_2px_color-mix(in_srgb,var(--accent)_16%,transparent)]"
              : "border-[var(--line)] border-b-[var(--line)] bg-[var(--surface-2)] text-[var(--paper)]",
          )}
        >
          {key}
        </kbd>
      ))}
    </span>
  );
}

export function PermissionBadge({ state }: { state: PermissionState }) {
  const copy =
    state === "ready"
      ? "Ready"
      : state === "waiting"
        ? "Waiting"
        : state === "denied"
          ? "Blocked"
          : state === "restricted"
            ? "Managed"
            : "Needed";
  return (
    <span
      className={cx(
        "shrink-0 rounded-full px-2.5 py-1 text-[11px] font-bold",
        state === "ready" && "bg-[var(--tint-ok)] text-[var(--ok-ink)]",
        state === "waiting" && "bg-[var(--tint-warn)] text-[var(--accent-hover)]",
        (state === "denied" || state === "restricted") && "bg-[var(--tint-danger)] text-[var(--danger)]",
        state === "needed" && "bg-[var(--surface-1)] text-[var(--muted-2)]",
      )}
    >
      {copy}
    </span>
  );
}

export function ShortcutCard({
  title,
  chord,
  copy,
  os,
  activeKeys,
  active = false,
  seen = false,
}: {
  title: string;
  chord: string;
  copy: string;
  os: OS;
  activeKeys: string[];
  active?: boolean;
  seen?: boolean;
}) {
  const displayActiveKeys = active ? formatChord(chord, os) : activeKeys;
  return (
    <div
      className={cx(
        "h-full rounded-[var(--radius-lg)] border px-4 py-3.5 text-left transition-[background-color,border-color,box-shadow,transform] duration-150",
        active
          ? "scale-[1.02] border-[var(--accent)] bg-[var(--tint-accent)] shadow-[0_0_0_3px_color-mix(in_srgb,var(--accent)_16%,transparent)]"
          : seen
            ? "border-[var(--ok-ink)] bg-[var(--surface-2)]"
            : "border-[var(--line)] bg-[var(--surface-2)]",
      )}
    >
      <div className="mb-2 flex items-center justify-between gap-2">
        <span className="text-[12px] font-bold text-[var(--paper)]">{title}</span>
        {seen ? <PermissionBadge state="ready" /> : null}
      </div>
      <ChordKeysActive chord={chord} os={os} activeKeys={displayActiveKeys} />
      <p className="m-0 mt-2 text-[11px] leading-[1.3] text-[var(--muted-2)]">{copy}</p>
    </div>
  );
}

// A control that isn't a keyboard chord (push-to-talk hold, middle-click).
// Same card shape as ShortcutCard but the "key" is a plain label, no live test.
export function ControlCard({
  title,
  label,
  copy,
  active = false,
  seen = false,
}: {
  title: string;
  label: string;
  copy: string;
  active?: boolean;
  seen?: boolean;
}) {
  return (
    <div
      className={cx(
        "h-full rounded-[var(--radius-lg)] border px-4 py-3.5 text-left transition-[background-color,border-color,box-shadow,transform] duration-150",
        active
          ? "scale-[1.02] border-[var(--accent)] bg-[var(--tint-accent)] shadow-[0_0_0_3px_color-mix(in_srgb,var(--accent)_16%,transparent)]"
          : seen
            ? "border-[var(--ok-ink)] bg-[var(--surface-2)]"
            : "border-[var(--line)] bg-[var(--surface-2)]",
      )}
    >
      <div className="mb-2 flex items-center justify-between gap-2">
        <span className="text-[12px] font-bold text-[var(--paper)]">{title}</span>
        {seen ? <PermissionBadge state="ready" /> : null}
      </div>
      <kbd
        className={cx(
          "inline-grid min-h-[24px] place-items-center rounded-[var(--radius-sm)] border px-2.5 [font-family:var(--font-mono)] text-[11px] font-semibold leading-none transition-[background-color,border-color,transform] duration-150",
          active
            ? "scale-[1.05] border-[var(--accent)] border-b-[var(--accent-hover)] bg-[var(--tint-accent)] text-[var(--paper)]"
            : "border-[var(--line)] border-b-[var(--line)] bg-[var(--surface-2)] text-[var(--paper)] shadow-[0_1px_0_var(--line)]",
        )}
      >
        {label}
      </kbd>
      <p className="m-0 mt-2 text-[11px] leading-[1.3] text-[var(--muted-2)]">{copy}</p>
    </div>
  );
}
