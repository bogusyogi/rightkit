import { useCallback, useEffect, useLayoutEffect, useRef, useState, type PointerEvent as ReactPointerEvent } from "react";
import { errorMessage } from "../../lib/engine.js";
import { type ShortProposal, type ShortsResult, type ReframeSummary, framingApproval, settingsGet, shortsAccept, shortsPropose, sourceList, thumbnails } from "../../lib/media.js";
import { readCurrentFraming, useFramingPlanChanges } from "./useVideoFramingReview.js";
import { assetUrl } from "../../lib/asset.js";
import { appendClip, projectShow, type ProjectShow } from "../../lib/project.js";
export type { ShortProposal, ShortsResult, ReframeSummary } from "../../lib/media.js";
type SourceRow = { id: string; name: string; path: string; exists: boolean; has_transcript: boolean };
const activeWrites = new Set<string>();
export const SHORT_ASPECT = "9:16";
function needsFraming(show: ProjectShow | null, source: string): boolean {
  const v = show?.timeline.sources.find((t) => t.id === source)?.video;
  return !!v && Math.abs(v.width / v.height / (9 / 16) - 1) >= 0.05;
}
export function useVideoShorts({dir, onChanged, refreshChangedProject, onExported, onAdded}: {dir: string; onChanged: () => Promise<void>; refreshChangedProject?: (dir: string) => Promise<void>; onExported: (result: Awaited<ReturnType<typeof shortsAccept>>) => void; onAdded: (proposal: ShortProposal) => void}) {
  const drawerSequence = useRef(0);
  const proposals = useRef<ReadonlySet<ShortProposal>>(new Set());
  const lifetime = useRef<{dir: string; source: string; generation: number} | null>(null);
  const [sources, setSources] = useState<SourceRow[]>([]);
  const [source, setSourceState] = useState("");
  const setSource = (next: string) => {
    const token = lifetime.current;
    if (token && token.source !== next) { token.source = next; token.generation += 1; proposals.current = new Set(); }
    setSourceState(next);
  };
  const [busy, setBusy] = useState<string | null>(null);
  // Which operation `busy` (a short id) stands for, so each button shows its own busy text.
  const [busyOp, setBusyOp] = useState<"add" | "export" | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [shorts, setShorts] = useState<ShortsResult | null>(null);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [open, setOpen] = useState<ShortProposal | null>(null);
  const [trim, setTrim] = useState<{ start: number; end: number } | null>(null);
  const [searched, setSearched] = useState(false);
  const [show, setShow] = useState<ProjectShow | null>(null);
  const [plan, setPlan] = useState<ReframeSummary | "none" | undefined>(undefined);
  const [autoFraming, setAutoFraming] = useState(false);
  const [framing, setFraming] = useState(false);
  const [framingScope, setFramingScope] = useState<object | null>(null);
  const scopeRef = useRef<object | null>(null);

  useLayoutEffect(() => {
    const token = { dir, source: "", generation: 0 };
    lifetime.current = token;
    setSources([]);
    setSourceState("");
    setShow(null);
    setPlan(undefined);
    setError(null);
    setFraming(false);
    setAutoFraming(false);
    return () => { if (lifetime.current === token) lifetime.current = null; };
  }, [dir]);

  const refresh = useCallback(async () => {
    const token = lifetime.current;
    const [s, p, settings] = await Promise.all([sourceList(dir), projectShow(dir), settingsGet(dir)]);
    if (!token || lifetime.current !== token) return;
    setAutoFraming(framingApproval(settings) === "auto");
    const rows = s.sources as SourceRow[];
    setSources(rows);
    setShow(p);
    setSourceState((cur) => cur || rows.find((x) => x.has_transcript)?.id || rows[0]?.id || "");
  }, [dir]);

  useEffect(() => {
    const token = lifetime.current;
    refresh().catch((e) => { if (token && lifetime.current === token) setError(errorMessage(e)); });
  }, [refresh]);

  useLayoutEffect(() => {
    const token = lifetime.current;
    if (!token) return;
    token.source = source; token.generation += 1;
    proposals.current = new Set();
    const scope = {};
    scopeRef.current = scope;
    setFramingScope(scope);
    setShorts(null);
    setSelected(new Set());
    setOpen(null);
    setTrim(null);
    setSearched(false);
    setBusy(null);
    setBusyOp(null);
    setPlan(undefined);
    setError(null);
    setFraming(false);
  }, [dir, source]);

  const selectedSource = sources.find((s) => s.id === source);
  const framingNeeded = needsFraming(show, source);

  // The vertical plan for the chosen source; "none" until one is analysed.
  useEffect(() => {
    setPlan(undefined);
    if (!source || !framingNeeded) return;
    let live = true;
    const token = lifetime.current;
    const generation = token?.generation;
    const current = () => live && !!token && lifetime.current === token && token.generation === generation && token.dir === dir;
    void readCurrentFraming(dir, source, SHORT_ASPECT,
      (plan) => { if (current()) setPlan(plan); },
      () => { if (current()) setPlan("none"); },
    );
    return () => {
      live = false;
    };
  }, [dir, source, framingNeeded]);

  const framingApproved = !framingNeeded || (plan !== undefined && plan !== "none" && plan.approved);
  const framingAllowed = autoFraming || framingApproved;

  const propose = async () => {
    const token = lifetime.current;
    if (!token || token.dir !== dir || token.source !== source || activeWrites.has(dir)) return;
    activeWrites.add(dir);
    const generation = token.generation;
    const current = () => lifetime.current === token && token.generation === generation;
    setBusy("propose");
    setError(null);
    try {
      const r = await shortsPropose(dir, source, 6);
      if (!current()) return;
      proposals.current = new Set(r.proposals);
      setShorts(r);
      setOpen(null);
      setTrim(null);
      setSelected(new Set());
      setSearched(true);
    } catch (e) {
      if (current()) setError(errorMessage(e));
    } finally {
      activeWrites.delete(dir);
      if (current()) setBusy(null);
    }
  };

  const exportShort = async (p: ShortProposal, bounds?: { start: number; end: number }, batch = false) => {
    const token = lifetime.current;
    if (!token || token.dir !== dir || token.source !== source || !framingAllowed || !proposals.current.has(p)) return false;
    const generation = token.generation;
    const current = () => lifetime.current === token && token.generation === generation;
    if (!current()) return false;
    setBusy(p.id);
    setBusyOp("export");
    setError(null);
    let written = false;
    let succeeded = false;
    try {
      const trimmed =
        bounds && (bounds.start !== p.start_ms || bounds.end !== p.end_ms)
          ? { start_ms: bounds.start, end_ms: bounds.end }
          : undefined;
      const r = await shortsAccept(dir, source, p.id, trimmed);
      written = true;
      if (!current()) return false;
      onExported(r);
      if (framingNeeded) {
        await readCurrentFraming(dir, source, SHORT_ASPECT, (plan) => { if (current()) setPlan(plan); });
        if (!current()) return false;
      }
      await refresh();
      if (!current()) return false;
      succeeded = current();
    } catch (e) {
      if (current()) setError(errorMessage(e));
    } finally {
      if (written) {
        try { await (refreshChangedProject ? refreshChangedProject(dir) : current() ? onChanged() : Promise.resolve()); }
        catch (e) { if (current()) setError(errorMessage(e)); succeeded = false; }
      }
      if (current() && !batch) { setBusy(null); setBusyOp(null); }
    }
    return succeeded && current();
  };

  const exportOne = async (p: ShortProposal, bounds?: { start: number; end: number }) => {
    if (activeWrites.has(dir)) return false;
    activeWrites.add(dir);
    try { return await exportShort(p, bounds); } finally { activeWrites.delete(dir); }
  };
  const exportSelected = async () => {
    if (activeWrites.has(dir)) return;
    activeWrites.add(dir);
    try {
    const list = (shorts?.proposals ?? []).filter((p) => selected.has(p.id));
    for (const p of list) {
      // eslint-disable-next-line no-await-in-loop -- sequential: one render worker
      if (!(await exportShort(p, undefined, true))) break;
    }
    } finally {
      activeWrites.delete(dir);
      const token = lifetime.current;
      if (token && token.dir === dir && token.source === source) { setBusy(null); setBusyOp(null); }
    }
  };

  const addToTimeline = async (p: ShortProposal, startMs: number, endMs: number) => {
    const token = lifetime.current;
    if (!token || token.dir !== dir || token.source !== source || !proposals.current.has(p) || activeWrites.has(dir)) return;
    const generation = token.generation;
    const current = () => lifetime.current === token && token.generation === generation;
    activeWrites.add(dir);
    setBusy(p.id);
    setBusyOp("add");
    setError(null);
    let written = false;
    try {
      await appendClip(dir, source, startMs, endMs);
      written = true;
      if (!current()) return;
      onAdded(p);
    } catch (e) {
      if (current()) setError(errorMessage(e));
    } finally {
      if (written) {
        try { await (refreshChangedProject ? refreshChangedProject(dir) : current() ? onChanged() : Promise.resolve()); }
        catch (e) { if (current()) setError(errorMessage(e)); }
      }
      activeWrites.delete(dir);
      if (current()) { setBusy(null); setBusyOp(null); }
    }
  };

  const framingChanged = useCallback((value: ReframeSummary | "none" | undefined) => {
    const token = lifetime.current;
    if (token && token.dir === dir && token.source === source && framingScope === scopeRef.current) setPlan(value);
  }, [dir, source, framingScope]);
  useFramingPlanChanges(dir, (changedSource, aspect, value) => {
    if (changedSource === source && aspect === SHORT_ASPECT) framingChanged(value);
  });
  const openDrawer = (p: ShortProposal) => {
    const token = lifetime.current;
    if (!token || token.dir !== dir || token.source !== source || !proposals.current.has(p)) return;
    drawerSequence.current += 1;
    setOpen(p);
    setTrim({ start: p.start_ms, end: p.end_ms });
  };

  const setCurrentTrim = (value: { start: number; end: number }) => {
    const token = lifetime.current;
    if (token && token.dir === dir && token.source === source && open && proposals.current.has(open)) setTrim(value);
  };

  const toggleSelection = (id: string, checked: boolean) => {
    const token = lifetime.current;
    if (!token || token.dir !== dir || token.source !== source || ![...proposals.current].some((p) => p.id === id)) return;
    setSelected((previous) => {
      const next = new Set(previous);
      if (checked) next.add(id); else next.delete(id);
      return next;
    });
  };
  const closeDrawer = () => { drawerSequence.current += 1; setOpen(null); };
  const addOpenedToTimeline = () => {
    if (open) return addToTimeline(open, trim?.start ?? open.start_ms, trim?.end ?? open.end_ms);
  };
  const exportOpenedShort = async () => {
    const proposal = open;
    const sequence = drawerSequence.current;
    if (!proposal) return false;
    const ok = await exportOne(proposal, trim ?? undefined);
    if (ok && drawerSequence.current === sequence) setOpen((current) => current === proposal ? null : current);
    return ok;
  };

  return { toggleSelection, closeDrawer, addOpenedToTimeline, exportOpenedShort, sources, source, setSource, busy, busyOp, error, shorts, selected, setSelected, open, setOpen, trim, setTrim: setCurrentTrim, searched, plan, framingChanged, autoFraming, framing, setFraming, selectedSource, framingNeeded, framingAllowed, propose, exportShort: exportOne, exportSelected, addToTimeline, openDrawer };
}
export function useShortThumbnail(dir: string, source: string, atMs: number) {
  const [src, setSrc] = useState<string | undefined>();
  useEffect(() => {
    let cancelled = false; setSrc(undefined);
    thumbnails(dir, { source }, { at_ms: atMs, width: 360 }).then((r) => !cancelled && setSrc(r.frames[0] ? assetUrl(r.frames[0].path) : undefined)).catch(() => undefined);
    return () => { cancelled = true; };
  }, [dir, source, atMs]);
  return src;
}

/** Clamp a pointer position to a proposal's context window and minimum duration. */
export function shortTrimBounds(proposal: ShortProposal, start: number, end: number, which: "in" | "out", fraction: number) {
  const lo = Math.max(0, proposal.start_ms - 5000);
  const hi = proposal.end_ms + 5000;
  const ms = Math.round(lo + Math.max(0, Math.min(1, fraction)) * (hi - lo));
  return which === "in" ? { start: Math.min(ms, end - 250), end } : { start, end: Math.max(ms, start + 250) };
}

/** Pointer ownership for a two-handle trim track. */
export function useShortTrimDrag(proposal: ShortProposal, start: number, end: number, onChange: (start: number, end: number) => void) {
  const track = useRef<HTMLDivElement | null>(null);
  const cleanup = useRef<(() => void) | null>(null);
  useLayoutEffect(() => () => { cleanup.current?.(); }, []);
  const drag = (which: "in" | "out") => (event: ReactPointerEvent) => {
    event.preventDefault();
    const element = track.current;
    if (!element || element.getBoundingClientRect().width <= 0) return;
    cleanup.current?.();
    const rect = element.getBoundingClientRect();
    const target = event.currentTarget as HTMLElement;
    const pointer = event.pointerId;
    target.setPointerCapture(pointer);
    const move = (e: PointerEvent) => {
      if (e.pointerId !== pointer) return;
      const next = shortTrimBounds(proposal, start, end, which, (e.clientX - rect.left) / rect.width);
      onChange(next.start, next.end);
    };
    const stop = (e: PointerEvent) => { if (e.pointerId === pointer) cleanup.current?.(); };
    const endDrag = () => {
      target.removeEventListener("pointermove", move);
      target.removeEventListener("pointerup", stop);
      target.removeEventListener("pointercancel", stop);
      target.removeEventListener("lostpointercapture", stop);
      if (target.hasPointerCapture(pointer)) target.releasePointerCapture(pointer);
      cleanup.current = null;
    };
    cleanup.current = endDrag;
    target.addEventListener("pointermove", move);
    target.addEventListener("pointerup", stop);
    target.addEventListener("pointercancel", stop);
    target.addEventListener("lostpointercapture", stop);
  };
  return { track, drag };
}
