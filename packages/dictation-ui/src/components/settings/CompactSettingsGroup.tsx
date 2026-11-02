import type { ReactNode } from "react";
import { SettingRow as RightKitSettingRow, SettingsGroup } from "@rightkit/settings-ui";
import { cx } from "../../lib/ui.js";

// Grouped inset-row form grammar (System Settings style, per the redesign
// mockup): a quiet sentence-case label sits ABOVE the card, rows divide by
// inset hairlines inside the card, and an optional explanation lives in the
// footer UNDER the group instead of in a paragraph at the top of the page.

export function CompactSettingsGroup({
  title,
  aside,
  footer,
  children,
  className,
  id,
  settingId,
}: {
  title: string;
  // Optional right-aligned content on the section header line (a Pro tag, a
  // "Show All" link, a count) — the mockup's `.sh small`/`a` slot.
  aside?: ReactNode;
  // Optional muted note rendered under the group — the mockup's `.foot`,
  // where per-group explanations live.
  footer?: ReactNode;
  children: ReactNode;
  className?: string;
  // Anchor target for deep links (e.g. the sidebar iPhone Sync row scrolls
  // to the Apple Sync group).
  id?: string;
  // Settings-search target: `data-setting-id` on the outer element so the
  // palette can scroll the group into view (e.g. "settings.appearance").
  settingId?: string;
}) {
  return (
    <section id={id} data-setting-id={settingId} className={cx("min-w-0", className)}>
      <div className="flex items-baseline justify-between gap-[var(--space-3)]">
        <div className="min-w-0 flex-1">
          <SettingsGroup title={title} description={footer}>
            {children}
          </SettingsGroup>
        </div>
        {aside}
      </div>
    </section>
  );
}

export function CompactSettingRow({
  label,
  sublabel,
  settingId,
  children,
  className,
}: {
  label: string;
  // One-line muted explanation under the label — the mockup's `.lab small`.
  sublabel?: string;
  // Settings-search target: `data-setting-id` on the outer row element
  // (e.g. "settings.show-pill").
  settingId?: string;
  children: ReactNode;
  className?: string;
}) {
  return (
    <div className={className}>
      <RightKitSettingRow id={settingId} label={label} description={sublabel}>
        {children}
      </RightKitSettingRow>
    </div>
  );
}
