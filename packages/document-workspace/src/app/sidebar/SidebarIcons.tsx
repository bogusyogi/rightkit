// Small icons used across the sidebar shell (header/footer + tab strip).
// Now backed by Phosphor React icons (see docs/agent-rules.md chrome modernization) instead of
// hand-rolled inline SVG — kept as named wrapper exports so call sites (SidebarExpandedPanel,
// SidebarRail) don't need to change.
import { Clock } from "@phosphor-icons/react/dist/icons/Clock";
import { Plus } from "@phosphor-icons/react/dist/icons/Plus";
import { SidebarSimple } from "@phosphor-icons/react/dist/icons/SidebarSimple";

export function ClockIcon() {
  return <Clock size={14} weight="regular" />;
}

export function PlusIcon() {
  return <Plus size={14} weight="regular" />;
}

// Standard "toggle sidebar" glyph — used for both open and collapse.
export function PanelIcon() {
  return <SidebarSimple size={14} weight="regular" />;
}
