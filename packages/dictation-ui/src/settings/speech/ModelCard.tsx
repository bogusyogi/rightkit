import { Fragment, type ReactNode } from "react";
import ErrorBanner from "../../components/ErrorBanner.js";
import { cx, ui } from "../../lib/ui.js";
import { BOX_BASE } from "./options.js";

// Shared skeleton for every model card in Speech recognition settings: the
// bordered box, the muted (!pro) opacity, the full-card downloading overlay,
// and the dismissible error banner. The active lane is a neutral elevated
// surface (no ember wash — ember is reserved for the active sidebar row,
// switches, primary buttons, and recording). Header, detail, and chip
// content are owned by the caller since Parakeet cards select on the whole
// header while the Whisper card selects per-language-chip.
export function ModelCard({
  active,
  muted,
  overlayActive,
  overlayLabel,
  error,
  onDismissError,
  children,
}: {
  active: boolean;
  muted?: boolean;
  overlayActive: boolean;
  overlayLabel: string;
  error: string;
  onDismissError: () => void;
  children: ReactNode;
}) {
  return (
    <div
      className={cx(
        BOX_BASE,
        active
          ? "border-[color-mix(in_srgb,var(--paper),transparent_55%)] bg-[var(--surface-2)]"
          : "border-[var(--line)] bg-transparent",
        muted && "opacity-90",
      )}
    >
      {children}

      {error ? (
        <div className="mt-[var(--space-2)]">
          <ErrorBanner message={error} onDismiss={onDismissError} />
        </div>
      ) : null}

      {overlayActive ? (
        <div className="absolute inset-0 grid place-items-center bg-[color-mix(in_srgb,var(--hr-ink),transparent_8%)] px-[var(--space-4)] text-center backdrop-blur-[2px]">
          <div>
            <div className="mx-auto mb-[var(--space-2)] h-5 w-5 animate-spin rounded-full border-2 border-[color-mix(in_srgb,var(--accent-cool),transparent_55%)] border-t-[var(--accent-cool)]" />
            <p
              className="m-0 text-[length:var(--size-sm)] font-medium text-[var(--paper)]"
              aria-live="polite"
            >
              {overlayLabel}
            </p>
          </div>
        </div>
      ) : null}
    </div>
  );
}

// Header shared by every model card so both lanes line up: title + "Powered
// by" line on the left, ONE status/action slot on the right, and the detail
// line full width below (Active pill, muted "Ready", a compact action button, or a Pro badge).
// `onSelect` makes the left block a button (Parakeet selects on the header;
// Whisper selects per language chip, so it passes none).
export function ModelCardHeader({
  title,
  summary,
  detail,
  status,
  onSelect,
  selectDisabled,
  selected,
}: {
  title: string;
  summary: string;
  detail: string;
  status: ReactNode;
  onSelect?: () => void;
  selectDisabled?: boolean;
  selected?: boolean;
}) {
  const body = (
    <>
      <span className="block text-[length:var(--size-md)] font-semibold leading-[1.3] text-[var(--paper)]">
        {title}
      </span>
      <span className="mt-[1px] block text-[length:var(--size-xs)] text-[var(--muted)]">
        {summary}
      </span>
    </>
  );
  return (
    <div>
      <div className="flex items-start justify-between gap-[var(--space-3)]">
        {onSelect ? (
          <button
            type="button"
            disabled={selectDisabled}
            aria-pressed={selected}
            onClick={onSelect}
            className="min-w-0 flex-1 cursor-pointer text-left focus-visible:outline-none focus-visible:[box-shadow:var(--focus-ring)] disabled:cursor-not-allowed"
          >
            {body}
          </button>
        ) : (
          <div className="min-w-0 flex-1">{body}</div>
        )}
        <div className="flex min-h-[26px] shrink-0 items-center">{status}</div>
      </div>
      {/* The detail spans the full card width, under the title row, so the action button never squeezes it. */}
      <p className="m-0 mt-[var(--space-2)] text-[length:var(--size-xs)] leading-snug text-[var(--muted)]">{detail}</p>
    </div>
  );
}

// Status vocabulary for the header slot: plain state, never a button.
export function ModelStatus({ state }: { state: "" | "checking" | "ready" | "active" }) {
  if (state === "") return null;
  if (state === "active") {
    return (
      <span className="rounded-full bg-[var(--surface-3)] px-[9px] py-[2px] text-[length:var(--size-xs)] font-semibold text-[var(--paper)]">
        Active
      </span>
    );
  }
  return (
    <span className="text-[length:var(--size-xs)] text-[var(--muted)]">
      {state === "checking" ? "Checking…" : "Ready"}
    </span>
  );
}

// Action vocabulary for the header slot: a compact secondary button.
export function ModelAction({
  children,
  onClick,
}: {
  children: ReactNode;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={cx(ui.button, ui.buttonSecondary, ui.buttonCompact, "whitespace-nowrap")}
    >
      {children}
    </button>
  );
}

// "Pro" lock badge for the header slot; opens pricing.
export function ProBadge({ onClick }: { onClick: () => void }) {
  return (
    <button
      type="button"
      onClick={onClick}
      className="cursor-pointer rounded-[5px] bg-[color-mix(in_srgb,var(--accent),transparent_82%)] px-[8px] py-[3px] text-[length:var(--size-xs)] font-bold uppercase tracking-[0.14em] text-[var(--accent)] focus-visible:outline-none focus-visible:[box-shadow:var(--focus-ring)]"
    >
      Pro
    </button>
  );
}

// Language chip. `plain` chips are informational (filled, no border);
// selectable chips share the same size/shape but are bordered buttons.
export const chipBase =
  "rounded-full px-[10px] py-[3px] text-[length:var(--size-xs)] leading-[1.4]";
export const chipInfo = cx(chipBase, "bg-[var(--surface-3)] text-[var(--paper)]");

// Language/model chip row shared by every card: capped at 5 entries with a
// "+N more" expander that toggles to the full list and back to "Show less".
export function ChipRow<T>({
  items,
  expanded,
  onToggleExpanded,
  keyOf,
  renderItem,
}: {
  items: T[];
  expanded: boolean;
  onToggleExpanded: () => void;
  keyOf: (item: T) => string;
  renderItem: (item: T) => ReactNode;
}) {
  const shown = expanded ? items : items.slice(0, 5);
  return (
    <div className="mt-[var(--space-3)] flex flex-wrap gap-[6px]">
      {shown.map((item) => (
        <Fragment key={keyOf(item)}>{renderItem(item)}</Fragment>
      ))}
      {items.length > 5 ? (
        <button
          type="button"
          onClick={onToggleExpanded}
          className={cx(chipBase, "cursor-pointer border border-[var(--line)] text-[var(--muted)] transition-[background-color,border-color] duration-150 ease-[var(--ease-out-strong)] hover:bg-[var(--surface-hover)] focus-visible:outline-none focus-visible:[box-shadow:var(--focus-ring)]")}
        >
          {expanded ? "Show less" : `+${items.length - 5} more`}
        </button>
      ) : null}
    </div>
  );
}
