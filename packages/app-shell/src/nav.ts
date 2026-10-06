import type { ReactNode } from "react";

export interface NavItem {
  id: string;
  label: string;
  icon?: ReactNode;
  /** Right-aligned tag, count, or status dot. */
  badge?: ReactNode;
  /** Extra Jump-to / palette search terms. */
  keywords?: readonly string[];
  /** Hide without removing (platform-only destinations). */
  hidden?: boolean;
}

export interface NavGroup {
  /** Optional: an untitled group renders without a heading (HeardRight's top group). */
  title?: string;
  items: readonly NavItem[];
}

function visibleItems(group: NavGroup): NavItem[] {
  return group.items.filter((item) => !item.hidden);
}

/** Jump-to filter: case-insensitive match on label or keywords; empty groups drop out. */
export function filterNavGroups(groups: readonly NavGroup[], query: string): NavGroup[] {
  const q = query.trim().toLowerCase();
  return groups
    .map((group) => ({
      ...group,
      items: visibleItems(group).filter(
        (item) =>
          !q ||
          item.label.toLowerCase().includes(q) ||
          (item.keywords ?? []).some((keyword) => keyword.toLowerCase().includes(q)),
      ),
    }))
    .filter((group) => group.items.length > 0);
}

export function firstNavItem(groups: readonly NavGroup[]): NavItem | undefined {
  return groups.map(visibleItems).find((items) => items.length > 0)?.[0];
}

export function flattenNav(groups: readonly NavGroup[]): NavItem[] {
  return groups.flatMap(visibleItems);
}

/**
 * Roving index for sidebar arrow-key navigation (HeardRight): vertical sidebars use Up/Down,
 * the narrow horizontal rail uses Left/Right; Home/End jump. Returns null when the key is not
 * a navigation key.
 */
export function navNeighborIndex(
  orientation: "horizontal" | "vertical",
  key: string,
  currentIndex: number,
  itemCount: number,
): number | null {
  if (itemCount <= 0) return null;
  if (key === "Home") return 0;
  if (key === "End") return itemCount - 1;
  const next = orientation === "horizontal" ? "ArrowRight" : "ArrowDown";
  const prev = orientation === "horizontal" ? "ArrowLeft" : "ArrowUp";
  if (key === next) return (currentIndex + 1) % itemCount;
  if (key === prev) return (currentIndex - 1 + itemCount) % itemCount;
  return null;
}
