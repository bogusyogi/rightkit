import { useState } from "react";
import type { DragEvent } from "react";
import { removeSavedSearch as removeSavedSearchFrom } from "../lib/savedSearches.js";
import type { SavedSearch } from "../store/types.js";

export function buildLabelTree<T extends { id: string; name: string }>(
  labels: readonly T[], hidden: ReadonlySet<string>, collapsed: ReadonlySet<string>,
) {
  const visibleLabels = labels.filter((label) => !hidden.has(label.id)).sort((a, b) => a.name.localeCompare(b.name));
  const parentNames = new Set<string>();
  for (const label of visibleLabels) {
    const parts = label.name.split("/");
    for (let i = 1; i < parts.length; i++) parentNames.add(parts.slice(0, i).join("/"));
  }
  const treeLabels = visibleLabels.filter((label) => {
    const parts = label.name.split("/");
    for (let i = 1; i < parts.length; i++) {
      if (collapsed.has(parts.slice(0, i).join("/"))) return false;
    }
    return true;
  });
  return { visibleLabels, parentNames, treeLabels };
}

/** The host owns presentation, selection policy, storage and transfer format. */
export interface SidebarModelOptions<T> {
  setCollapsedLabels: (update: (previous: Set<string>) => Set<string>) => void;
  setSavedSearches: (update: (previous: SavedSearch[]) => SavedSearch[]) => void;
  persistCollapsedLabels: (value: string[]) => void;
  persistSavedSearches: (value: SavedSearch[]) => void;
  transferType: string;
  decodeTransfer?: (raw: string) => T;
  onDrop: (value: T, labelId: string, labelName: string) => void;
  onInvalidDrop: () => void;
  onSelectFolder: (id: string) => void;
}

/** Headless interactions for a host-composed sidebar. */
export function useSidebarModel<T>({ setCollapsedLabels, setSavedSearches, persistCollapsedLabels, persistSavedSearches, transferType, decodeTransfer, onDrop, onInvalidDrop, onSelectFolder }: SidebarModelOptions<T>) {
  const [dropLabelId, setDropLabelId] = useState<string | null>(null);
  function toggleLabelCollapsed(name: string) {
    setCollapsedLabels((prev) => {
      const next = new Set(prev);
      if (next.has(name)) next.delete(name);
      else next.add(name);
      persistCollapsedLabels([...next]);
      return next;
    });
  }
  function removeSavedSearch(query: string) {
    setSavedSearches((prev) => {
      const next = removeSavedSearchFrom(prev, query);
      persistSavedSearches(next);
      return next;
    });
  }
  function labelDropHandlers(labelId: string, labelName: string) {
    return {
      onDragEnter: (e: DragEvent<HTMLButtonElement>) => {
        if (e.dataTransfer.types.includes(transferType)) setDropLabelId(labelId);
      },
      onDragLeave: (e: DragEvent<HTMLButtonElement>) => {
        if (!e.currentTarget.contains(e.relatedTarget as Node | null)) {
          setDropLabelId((current) => current === labelId ? null : current);
        }
      },
      onDragOver: (e: DragEvent<HTMLButtonElement>) => {
        if (e.dataTransfer.types.includes(transferType)) {
          e.preventDefault();
          e.dataTransfer.dropEffect = "move";
        }
      },
      onDrop: (e: DragEvent<HTMLButtonElement>) => {
        const raw = e.dataTransfer.getData(transferType);
        if (!raw) return;
        e.preventDefault();
        e.stopPropagation();
        let value: T;
        try { value = decodeTransfer ? decodeTransfer(raw) : JSON.parse(raw) as T; }
        catch { onInvalidDrop(); return; }
        setDropLabelId(null);
        onDrop(value, labelId, labelName);
      },
    };
  }
  return { dropLabelId, toggleLabelCollapsed, removeSavedSearch, labelDropHandlers, openFolderItem: onSelectFolder };
}
