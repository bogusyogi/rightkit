import { useMemo, useRef } from "react";
import { ContextMenu, type AnchorRect, type MenuItem } from "@rightkit/react-shell/react";
import { useSettings } from "@rightkit/document-core/settings/settings";
import { BASE_THEME_IDS, themeById } from "@rightkit/document-core/settings/themes";
import { menuPanelClass, menuItemClass } from "../app/sidebar/SidebarPrimitives";

// Auto is a two-tone light/dark pie; every theme is a pie of its ink / primary / paper.
const AUTO_SWATCH = ["#F7F4EE", "#121417"];
const TRIGGER = '[data-vr-sidebar="expanded"] button[aria-label="Theme"]';

/** Compact theme menu from the sidebar footer. The interface font lives in Settings → Typography
 *  (a quick menu shouldn't restyle every control); "More settings…" opens the full panel.
 *  Call sites may pass `anchor` (the Theme button); otherwise the menu finds that button. */
export function AppearanceMenu({
  onClose,
  onMore,
  anchor,
}: {
  onClose: () => void;
  onMore: () => void;
  anchor?: Element | AnchorRect;
}) {
  const { settings, update } = useSettings();
  // `update` / `onMore` may change identity per render; items must not (exitMs treats a new items
  // identity as a re-open), so run() reads the latest through a ref.
  const latest = useRef({ update, onMore });
  latest.current = { update, onMore };
  const themeId = settings.themeId;

  const presets = useMemo(() => [
    { id: "auto", name: "Auto", note: "Follows system", swatch: AUTO_SWATCH },
    ...BASE_THEME_IDS.map((id) => {
      const t = themeById(id);
      return { id: t.id, name: t.name, note: "", swatch: [t.tokens.ink, t.tokens.primary, t.tokens.paper] };
    }),
  ], []);
  const byId = useMemo(() => new Map(presets.map((p) => [`preset:${p.id}`, p])), [presets]);
  const items: MenuItem[] = useMemo(
    () => [
      ...presets.map((p) => ({
        id: `preset:${p.id}`,
        label: p.name,
        checked: themeId === p.id,
        run: () => latest.current.update({ themeId: p.id }),
      })),
      { id: "more", label: "More settings…", separatorBefore: true, run: () => latest.current.onMore() },
    ],
    [presets, themeId],
  );

  return (
    <ContextMenu
      anchor={anchor ?? document.querySelector(TRIGGER) ?? { left: 8, top: window.innerHeight - 48, right: 248, bottom: window.innerHeight - 48 }}
      placement="top-end"
      label="Theme"
      width={240}
      items={items}
      onClose={onClose}
      itemAs="button"
      showLead={false}
      renderItem={(item) => {
        const preset = byId.get(item.id);
        if (!preset) return <span className="flex-1 text-center text-xs text-muted">{item.label}</span>;
        const active = item.checked;
        return (
          <>
            <Swatch colors={preset.swatch} />
            <span className="flex-1">{preset.name}</span>
            {preset.note && <span className="text-chrome-xs text-muted">{preset.note}</span>}
            {active && (
              <span aria-hidden="true" className="text-copper-hi">
                ✓
              </span>
            )}
          </>
        );
      }}
      className={`${menuPanelClass} p-2`}
      scrimClassName="no-print"
      exitMs={80}
      exitClassName="vr-pop-exit"
      slots={{
        item: `${menuItemClass} vr-focus h-9 gap-2.5 rounded-md px-2 text-chrome-md [&.is-active]:bg-ink/30 [&[aria-checked=true]]:bg-copper/15 [&[aria-checked=true]]:text-paper`,
        separator: "my-2 h-px",
      }}
    />
  );
}

/** Decorative pie of a theme's ink / primary / paper (Auto is a two-tone light/dark pie). RightKit
 *  has no swatch component; the menu row text carries the theme name. */
function Swatch({ colors }: { colors: string[] }) {
  const step = 100 / colors.length;
  const stops = colors.map((color, i) => `${color} ${i * step}% ${(i + 1) * step}%`).join(", ");
  return (
    <span
      aria-hidden="true"
      className="h-5 w-5 shrink-0 rounded-full border border-line"
      style={{ background: `conic-gradient(from 135deg, ${stops})` }}
    />
  );
}
