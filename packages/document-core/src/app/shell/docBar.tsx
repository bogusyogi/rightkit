// One docked document bar for every format: App renders a single top row (the active format's
// tools on the left, view tools on the right; the bottom status row carries path + stats + save
// state). Each format keeps owning its tool row but renders it through <DocBar>, which portals
// it into App's slot while that document's pane is active. Inactive (kept-alive) panes render
// nothing, and outside the App shell (unit tests, standalone views) the bar renders in place.
import { createContext, useContext, type ReactNode } from "react";
import { createPortal } from "react-dom";

/** App's centre slot element (null until mounted, or outside the shell). */
export const DocBarSlotContext = createContext<HTMLElement | null>(null);

/** Whether the surrounding document pane is the active one; true outside the router. */
export const PaneActiveContext = createContext(true);

export function DocBar({ children }: { children: ReactNode }) {
  const slot = useContext(DocBarSlotContext);
  const active = useContext(PaneActiveContext);
  const bar = <div data-vr-floating-bar="">{children}</div>;
  if (!slot) return bar;
  return active ? createPortal(bar, slot) : null;
}
