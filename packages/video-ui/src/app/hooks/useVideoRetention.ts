import { useCallback, useEffect, useLayoutEffect, useRef, useState, useSyncExternalStore } from "react";
import { errorMessage } from "../../lib/engine.js";
import { type RenderFile, type RetentionShow, mediaScopeAllow, rendersList, retentionApply, retentionImport, retentionList, retentionShow } from "../../lib/media.js";
import { assetUrl } from "../../lib/asset.js";
import { pickFile } from "../lib/dialog.js";

export type { DropOff } from "../../lib/media.js";
type ImportResult = Awaited<ReturnType<typeof retentionImport>>;
type ApplyResult = Awaited<ReturnType<typeof retentionApply>>;
type RetentionRow = Awaited<ReturnType<typeof retentionList>>["retention"][number];
type MutationResult = { kind: "import"; name: string } | { kind: "apply" };
type Lease = { kind: MutationResult["kind"]; done: Promise<void>; result?: MutationResult };
const leases = new Map<string, Lease>();
const listeners = new Set<() => void>();
const completed = new Set<(dir: string, writer: object, lease: Lease) => void>();
let snapshot: ReadonlyMap<string, Lease> = new Map();
const subscribe = (fn: () => void) => { listeners.add(fn); return () => { listeners.delete(fn); }; };
function publish() { snapshot = new Map(leases); for (const fn of listeners) fn(); }

export function useVideoRetention({ dir, preselectRender, onImported, onApplied, refreshChangedProject, pickTitle, readFileError }: {
  dir: string; preselectRender?: string;
  onImported: (result: ImportResult) => void; onApplied: (result: ApplyResult) => void;
  refreshChangedProject: (dir: string) => Promise<void>; pickTitle: string; readFileError: (file: string) => string;
}) {
  const [renders, setRenders] = useState<RenderFile[]>([]);
  const [imports, setImports] = useState<RetentionRow[]>([]);
  const [detail, setDetail] = useState<{ sequence: number; value: RetentionShow } | null>(null);
  const active = detail?.value ?? null;
  const [csv, setCsv] = useState("");
  const [render, setRender] = useState("");
  const [checked, setChecked] = useState<Set<string>>(new Set());
  const [readBusy, setReadBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const activeLeases = useSyncExternalStore(subscribe, () => snapshot, () => snapshot);
  const owner = useRef<{ dir: string; list: number; show: number; manual: boolean } | null>(null);
  useLayoutEffect(() => {
    const token = { dir, list: 0, show: 0, manual: false };
    owner.current = token;
    setRenders([]); setImports([]); setDetail(null); setCsv(""); setRender(""); setChecked(new Set()); setReadBusy(null); setError(null);
    return () => { if (owner.current === token) owner.current = null; };
  }, [dir]);
  const refresh = useCallback(async () => {
    const token = owner.current;
    if (!token || token.dir !== dir) return;
    const sequence = ++token.list;
    try {
      const [r, ret] = await Promise.all([rendersList(dir), retentionList(dir)]);
      if (owner.current !== token || token.list !== sequence) return;
      const mp4s = r.renders.filter((f) => f.name.endsWith(".mp4"));
      setRenders(mp4s); setImports(ret.retention); setRender((cur) => cur || mp4s[0]?.name || "");
      return { sequence, rows: ret.retention };
    } catch (e) { if (owner.current === token && token.list === sequence) setError(errorMessage(e)); }
  }, [dir]);
  useEffect(() => { if (preselectRender) setRender(preselectRender); }, [dir, preselectRender]);
  const show = useCallback(async (name: string, manual: boolean) => {
    const token = owner.current;
    if (!token || token.dir !== dir) return;
    if (manual) token.manual = true;
    const sequence = ++token.show;
    setReadBusy(`show-${name}`); setError(null);
    try {
      const value = await retentionShow(dir, name);
      if (owner.current !== token || token.show !== sequence) return;
      setDetail({ sequence, value }); setChecked(new Set((value.drop_offs ?? []).map((d) => d.id)));
    } catch (e) { if (owner.current === token && token.show === sequence) setError(errorMessage(e)); }
    finally { if (owner.current === token && token.show === sequence) setReadBusy(null); }
  }, [dir]);
  const open = useCallback((name: string) => { if (!leases.has(dir)) return show(name, true); }, [dir, show]);
  useEffect(() => {
    const token = owner.current;
    if (!token || token.dir !== dir) return;
    let cancelled = false;
    const initialLease = leases.get(dir);
    const current = () => !cancelled && owner.current === token;
    const reload = async (lease?: Lease) => {
      if (lease) await lease.done;
      if (!current()) return;
      const list = await refresh();
      if (!list || lease?.result?.kind === "apply") return;
      const canSelect = () => current() && list.sequence === token.list && !token.manual;
      while (canSelect()) {
        const successor = leases.get(dir);
        if (!successor) break;
        await successor.done;
      }
      if (!canSelect()) return;
      const name = lease?.result?.kind === "import" ? lease.result.name : list.rows[list.rows.length - 1]?.name;
      if (name) await show(name, false);
    };
    const onDone = (changedDir: string, writer: object, lease: Lease) => {
      if (!current() || changedDir !== dir || writer === token) return;
      token.show += 1;
      token.list += 1;
      setDetail(null); setChecked(new Set()); setReadBusy(null);
      if (lease !== initialLease) void reload(lease);
    };
    completed.add(onDone);
    void reload(initialLease);
    return () => { cancelled = true; completed.delete(onDone); };
  }, [dir, refresh, show]);
  const withLease = async (kind: Lease["kind"], work: (token: NonNullable<typeof owner.current>, lease: Lease) => Promise<void>) => {
    const token = owner.current;
    if (!token || token.dir !== dir || leases.has(dir)) return;
    let finish!: () => void;
    const done = new Promise<void>((resolve) => { finish = resolve; });
    const lease = { kind, done };
    leases.set(dir, lease); publish();
    try { await work(token, lease); }
    catch (e) { if (owner.current === token) setError(errorMessage(e)); }
    finally { leases.delete(dir); finish(); publish(); }
  };
  const importText = async (text: string, token: NonNullable<typeof owner.current>, lease: Lease) => {
    if (owner.current !== token || !text.trim() || !render) return;
    setError(null);
    const result = await retentionImport(dir, text, render);
    lease.result = { kind: "import", name: result.name };
    for (const fn of completed) fn(dir, token, lease);
    if (owner.current !== token) return;
    onImported(result); setCsv("");
    await refresh();
    if (owner.current === token) await show(result.name, true);
  };
  const doImport = (text: string) => withLease("import", (token, lease) => importText(text, token, lease));
  const importFile = () => withLease("import", async (token, lease) => {
    const file = await pickFile(pickTitle, ["csv", "txt"]);
    if (!file || owner.current !== token) return;
    await mediaScopeAllow([file]);
    if (owner.current !== token) return;
    const response = await fetch(assetUrl(file));
    if (!response.ok) throw new Error(readFileError(file));
    const text = await response.text();
    if (owner.current === token) await importText(text, token, lease);
  });
  const apply = () => withLease("apply", async (token, lease) => {
    if (!detail || detail.sequence !== token.show || detail.value.stale || checked.size === 0) return;
    setError(null);
    const result = await retentionApply(dir, detail.value.name, [...checked]);
    lease.result = { kind: "apply" };
    for (const fn of completed) fn(dir, token, lease);
    try {
      if (owner.current === token) { onApplied(result); setDetail(null); await refresh(); }
    } finally {
      await refreshChangedProject(dir);
    }
  });
  const setDropChecked = (id: string, value: boolean) => setChecked((previous) => {
    const next = new Set(previous); if (value) next.add(id); else next.delete(id); return next;
  });
  const busy = activeLeases.get(dir)?.kind ?? readBusy;
  return { renders, imports, active, csv, setCsv, render, setRender, checked, setDropChecked, busy, error, open, doImport, importFile, apply };
}
