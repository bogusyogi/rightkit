import { useCallback, useEffect, useState } from "react";

const STORAGE_KEY = "cr-palette-recent-v1";
const MAX_RECENTS = 5;

export type PaletteRecent = {
  key: string;
  label: string;
  sub?: string;
  scope: "global" | "slash" | "mention";
};

function readRecents(): PaletteRecent[] {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw) as unknown;
    if (!Array.isArray(parsed)) return [];
    return parsed
      .filter(
        (row): row is PaletteRecent =>
          !!row && typeof row === "object" && typeof (row as PaletteRecent).key === "string",
      )
      .slice(0, MAX_RECENTS);
  } catch {
    return [];
  }
}

function persistRecents(recents: PaletteRecent[]) {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(recents.slice(0, MAX_RECENTS)));
  } catch {
    /* ignore — recents stay in-memory for this session */
  }
}

/** Persist the last 5 palette launches in localStorage; most-recent first. */
export function usePaletteRecents() {
  const [recents, setRecents] = useState<PaletteRecent[]>(() => readRecents());

  useEffect(() => {
    persistRecents(recents);
  }, [recents]);

  const addRecent = useCallback((entry: PaletteRecent) => {
    setRecents((prev) => {
      const next = [entry, ...prev.filter((r) => r.key !== entry.key)].slice(0, MAX_RECENTS);
      return next;
    });
  }, []);

  const clearRecents = useCallback(() => {
    setRecents([]);
  }, []);

  return { recents, addRecent, clearRecents };
}
