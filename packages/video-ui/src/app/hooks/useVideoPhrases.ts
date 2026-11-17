import { useEffect, useLayoutEffect, useRef, useState, useSyncExternalStore } from "react";
import { errorMessage } from "../../lib/engine.js";
import { clipExport, type PhraseMatch, phraseSearch, sourceList } from "../../lib/media.js";
import { appendClip } from "../../lib/project.js";

export type VideoPhraseSource = { id: string; name: string; path: string; exists: boolean; has_transcript: boolean };
export type { PhraseMatch } from "../../lib/media.js";

const activeActions = new Set<string>();
const actionListeners = new Set<() => void>();
let actionSnapshot: ReadonlySet<string> = new Set();
function publishActions() {
  actionSnapshot = new Set(activeActions);
  for (const listener of actionListeners) listener();
}
function subscribeActions(listener: () => void) {
  actionListeners.add(listener);
  return () => { actionListeners.delete(listener); };
}

/** Search, preview selection & phrase actions, with host-owned notifications. */
export function useVideoPhrases({ dir, refreshChangedProject, onInserted, onExported, onActionError }: {
  dir: string;
  refreshChangedProject: (dir: string) => Promise<void>;
  onInserted: () => void;
  onExported: (name: string) => void;
  onActionError: (message: string) => void;
}) {
  const [sources, setSources] = useState<VideoPhraseSource[]>([]);
  const [filter, setFilter] = useState("");
  const [query, setQuery] = useState("");
  const [matches, setMatches] = useState<PhraseMatch[] | null>(null);
  const [searching, setSearching] = useState(false);
  const active = useSyncExternalStore(subscribeActions, () => actionSnapshot, () => actionSnapshot);
  const [error, setError] = useState<string | null>(null);
  const [preview, setPreview] = useState<PhraseMatch | null>(null);
  const lifetime = useRef<{ dir: string; search: number } | null>(null);
  useLayoutEffect(() => {
    const owner = { dir, search: 0 };
    lifetime.current = owner;
    setSources([]); setFilter(""); setQuery(""); setMatches(null);
    setSearching(false); setError(null); setPreview(null);
    return () => { if (lifetime.current === owner) lifetime.current = null; };
  }, [dir]);
  useEffect(() => {
    const owner = lifetime.current;
    if (!owner || owner.dir !== dir) return;
    let cancelled = false;
    sourceList(dir)
      .then((s) => { if (!cancelled && lifetime.current === owner) setSources(s.sources as VideoPhraseSource[]); })
      .catch((e) => { if (!cancelled && lifetime.current === owner) setError(errorMessage(e)); });
    return () => { cancelled = true; };
  }, [dir]);

  const search = async () => {
    const owner = lifetime.current;
    if (!owner || owner.dir !== dir || !query.trim()) return;
    const sequence = ++owner.search;
    const current = () => lifetime.current === owner && owner.search === sequence;
    setSearching(true); setError(null);
    try {
      const r = await phraseSearch(dir, query.trim());
      if (current()) setMatches(r.matches);
    } catch (e) {
      if (current()) setError(errorMessage(e));
    } finally {
      if (current()) setSearching(false);
    }
  };

  const actionKey = (kind: string, m: PhraseMatch) => JSON.stringify([dir, kind, m.source, m.word_index]);
  const perform = async (kind: "insert" | "export", m: PhraseMatch) => {
    const owner = lifetime.current;
    if (!owner || owner.dir !== dir) return;
    const key = actionKey(kind, m);
    if (activeActions.has(key)) return;
    activeActions.add(key); publishActions();
    try {
      if (kind === "insert") {
        await appendClip(dir, m.source, m.context.start_ms, m.context.end_ms);
        if (lifetime.current === owner) onInserted();
      } else {
        const result = await clipExport(dir, { source: m.source, start_ms: m.context.start_ms, end_ms: m.context.end_ms });
        if (lifetime.current === owner) onExported(result.name);
      }
      await refreshChangedProject(dir);
    } catch (e) {
      if (lifetime.current === owner) onActionError(errorMessage(e));
    } finally {
      activeActions.delete(key); publishActions();
    }
  };
  const insert = (m: PhraseMatch) => perform("insert", m);
  const exportClip = (m: PhraseMatch) => perform("export", m);
  const isInserting = (m: PhraseMatch) => active.has(actionKey("insert", m));
  const isExporting = (m: PhraseMatch) => active.has(actionKey("export", m));

  const srcById = new Map(sources.map((s) => [s.id, s]));
  const shown = (matches ?? []).filter((m) => !filter || m.source === filter);

  return { sources, filter, setFilter, query, setQuery, matches, searching, isInserting, isExporting, error, preview, setPreview, search, insert, exportClip, srcById, shown };
}

/** Locate the first case-insensitive query match for host-owned highlighting. */
export function phraseHighlight(text: string, query: string): { before: string; match: string; after: string } | null {
  const i = text.toLowerCase().indexOf(query.toLowerCase());
  if (i < 0 || !query.trim()) return null;
  return { before: text.slice(0, i), match: text.slice(i, i + query.length), after: text.slice(i + query.length) };
}
