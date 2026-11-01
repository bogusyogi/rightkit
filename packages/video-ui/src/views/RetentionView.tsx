// Retention — docs/design/app-ui.md §3.6. Audience-retention CSV bound to one
// exact render; drop-offs become cut proposals applied as one revision.
import { Badge, Button, Card, Checkbox, EmptyState, Field, PageShell, useToast } from "@rightkit/react-shell/react";
import "./Retention.css";
import { useCallback, useEffect, useRef, useState } from "react";
import { errorMessage } from "../lib/engine";
import {
  type DropOff,
  type RenderFile,
  type RetentionShow,
  mediaScopeAllow,
  rendersList,
  retentionApply,
  retentionImport,
  retentionList,
  retentionShow,
} from "../lib/media";
import { assetUrl } from "../lib/asset";
import { pickFile } from "../lib/dialog";
import { ErrorPanel, timecode, Icon } from "../ui";

type RetentionRow = { name: string; render?: string; points: number; drop_offs?: number; revision?: number };

/** Retention % line chart, inline SVG on tokens (spec §3.6). 160px tall. */
function RetentionChart({ points, drops }: { points: { out_ms: number; retention: number }[]; drops: DropOff[] }) {
  const [tip, setTip] = useState<{ x: number; y: number; text: string } | null>(null);
  const W = 640;
  const H = 160;
  const PAD = { l: 34, r: 8, t: 8, b: 18 };
  const iw = W - PAD.l - PAD.r;
  const ih = H - PAD.t - PAD.b;
  const maxMs = Math.max(...points.map((p) => p.out_ms), 1);
  const x = (ms: number) => PAD.l + (ms / maxMs) * iw;
  const y = (v: number) => PAD.t + (1 - Math.max(0, Math.min(100, v)) / 100) * ih;
  const path = points.map((p, i) => `${i === 0 ? "M" : "L"}${x(p.out_ms).toFixed(1)},${y(p.retention).toFixed(1)}`).join(" ");
  return (
    <div className="retention-chart">
      <svg viewBox={`0 0 ${W} ${H}`} preserveAspectRatio="none" role="img" aria-label="Retention over time">
        {[0, 25, 50, 75, 100].map((g) => (
          <g key={g}>
            <line x1={PAD.l} x2={W - PAD.r} y1={y(g)} y2={y(g)} stroke="var(--rk-line)" strokeWidth={1} />
            <text x={4} y={y(g) + 3} fill="var(--rk-ink-2)" fontSize={9} fontFamily="var(--rk-font-mono)">
              {g}
            </text>
          </g>
        ))}
        <path d={path} fill="none" stroke="var(--rk-accent)" strokeWidth={1.6} />
        {drops.map((d) => {
          const cx = x(d.out_start_ms);
          const near = points.reduce(
            (best, p) => (Math.abs(p.out_ms - d.out_start_ms) < Math.abs(best.out_ms - d.out_start_ms) ? p : best),
            points[0] ?? { out_ms: 0, retention: 50 },
          );
          return (
            <circle
              key={d.id}
              cx={cx}
              cy={y(near.retention)}
              r={4}
              fill="var(--rk-bad)"
              onMouseEnter={() =>
                setTip({ x: cx, y: y(near.retention), text: `${timecode(d.out_start_ms)} · −${d.drop_points.toFixed(1)} pts` })
              }
              onMouseLeave={() => setTip(null)}
            />
          );
        })}
      </svg>
      {tip && (
        <div className="retention-tip cr-mono" style={{ left: `min(${(tip.x / W) * 100}%, 80%)`, top: tip.y - 8 }}>
          {tip.text}
        </div>
      )}
      <div className="retention-chart__axis cr-muted">
        <span>0:00</span>
        <span className="cr-mono">{timecode(maxMs)}</span>
      </div>
    </div>
  );
}

export function RetentionView({
  dir,
  onChanged,
  onExportRevised,
  preselectRender,
}: {
  dir: string;
  onChanged: () => Promise<void>;
  onExportRevised?: () => void;
  /** R5: Exports "Import retention…" routes here with this render preselected. */
  preselectRender?: string;
}) {
  const toast = useToast();
  const [renders, setRenders] = useState<RenderFile[]>([]);
  const [imports, setImports] = useState<RetentionRow[]>([]);
  const [active, setActive] = useState<RetentionShow | null>(null);
  const [csv, setCsv] = useState("");
  const [render, setRender] = useState("");
  const [checked, setChecked] = useState<Set<string>>(new Set());
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const dirty = useRef(false);

  const refresh = useCallback(async () => {
    const [r, ret] = await Promise.all([rendersList(dir), retentionList(dir)]);
    const mp4s = r.renders.filter((f) => f.name.endsWith(".mp4"));
    setRenders(mp4s);
    setImports(ret.retention);
    setRender((cur) => cur || mp4s[0]?.name || "");
  }, [dir]);

  useEffect(() => {
    refresh().catch((e) => setError(errorMessage(e)));
  }, [refresh]);

  // Route in from Exports with a specific render preselected.
  useEffect(() => {
    if (preselectRender) setRender(preselectRender);
  }, [preselectRender]);

  const open = useCallback(
    async (name: string) => {
      setBusy(`show-${name}`);
      setError(null);
      try {
        const s = await retentionShow(dir, name);
        setActive(s);
        setChecked(new Set((s.drop_offs ?? []).map((d) => d.id)));
      } catch (e) {
        setError(errorMessage(e));
      } finally {
        setBusy(null);
      }
    },
    [dir],
  );

  // Auto-open the newest import (but don't clobber the user's selection).
  useEffect(() => {
    if (!active && !dirty.current && imports.length > 0) {
      void open(imports[imports.length - 1].name);
    }
  }, [imports, active, open]);

  const doImport = async (text: string) => {
    if (!text.trim() || !render) return;
    setBusy("import");
    setError(null);
    try {
      const r = await retentionImport(dir, text, render);
      toast(`Imported ${r.points} retention points — ${r.drop_offs} drop-off proposals`, { kind: "success" });
      setCsv("");
      await refresh();
      await open(r.name);
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(null);
    }
  };

  const importFile = async () => {
    const file = await pickFile("Import retention CSV", ["csv", "txt"]);
    if (!file) return;
    setBusy("import");
    try {
      await mediaScopeAllow([file]);
      const text = await fetch(assetUrl(file)).then((r) => {
        if (!r.ok) throw new Error(`could not read ${file}`);
        return r.text();
      });
      await doImport(text);
    } catch (e) {
      setError(errorMessage(e));
      setBusy(null);
    }
  };

  const apply = async () => {
    if (!active) return;
    setBusy("apply");
    setError(null);
    try {
      const res = await retentionApply(dir, active.name, [...checked]);
      toast(`Applied ${res.cuts} cut${res.cuts === 1 ? "" : "s"} — re-export to deliver`, { kind: "success", action: onExportRevised ? { label: "Export revised", run: onExportRevised } : undefined });
      dirty.current = true;
      setActive(null);
      await refresh();
      await onChanged();
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(null);
    }
  };

  const drops = active?.drop_offs ?? [];

  return (
    <PageShell title="Retention" maxWidth={0} subtitle={"Audience drop-offs mapped onto your exports"}>
      <div className="retention-body">
        <div className="retention-controls">
          <Field label="Export">{(ids) => (<select {...ids} className="rk-input cr-select" value={render} onChange={(e) => setRender(e.target.value)}>
              {renders.length === 0 && <option value="">No exports yet</option>}
              {renders.map((r) => (
                <option key={r.path} value={r.name}>
                  {r.name}
                </option>
              ))}
            </select>)}</Field>
          <Button variant="secondary" disabled={!render || busy !== null} onClick={() => void importFile()}><Icon name="upload" />
            Import CSV…
          </Button>
          <span className="cr-muted retention-controls__or">or paste <span className="cr-mono">time,retention</span> rows:</span>
        </div>
        <textarea
          className="rk-input cr-textarea"
          placeholder={"time,retention\n0,100\n1,97\n5,72"}
          value={csv}
          onChange={(e) => setCsv(e.target.value)}
        />
        <div>
          <Button
            variant="secondary"
            onClick={() => void doImport(csv)}
            disabled={(!csv.trim() || !render) || (busy === "import")}
            aria-busy={(busy === "import") || undefined}
          >
            {busy === "import" ? "Importing…" : "Import"}
          </Button>
        </div>

        {error && <ErrorPanel title="Retention failed" detail={error} />}

        {imports.length > 0 && (
          <div className="retention-imports">
            {imports.map((r) => (
              <Button variant="ghost" size="sm" className="cr-chip-btn" key={r.name} onClick={() => void open(r.name)} title={`${r.points} points${r.drop_offs != null ? ` · ${r.drop_offs} drop-offs` : ""}`}><Badge
                tone={active?.name === r.name ? "accent" : "neutral"}
              >
                {r.name}
              </Badge></Button>
            ))}
          </div>
        )}

        {active?.stale && (
          <ErrorPanel
            title="This analysis is stale"
            detail={
              active.render_exists
                ? `The export ${active.render} changed since this CSV was imported. Re-import the retention data against the current file.`
                : `The export ${active.render} no longer exists. Re-render, then import the retention data again.`
            }
          />
        )}

        {active && active.points.length > 0 && (
          <Card>
            <RetentionChart points={active.points} drops={drops.filter((d) => checked.has(d.id))} />
          </Card>
        )}

        {active && drops.length > 0 && (
          <div className="retention-drops">
            <span className="cr-overline">Drop-off cuts</span>
            {drops.map((d) => (
              <Checkbox
                key={d.id}
                className="retention-drop"
                checked={checked.has(d.id)}
                onChange={(checked) =>
                  setChecked((s) => {
                    const n = new Set(s);
                    if (checked) n.add(d.id);
                    else n.delete(d.id);
                    return n;
                  })
                }
                label={
                  <span style={{ display: "flex", alignItems: "center", gap: "var(--s3)" }}>
                    <span className="cr-mono cr-muted">
                      {timecode(d.out_start_ms)}–{timecode(d.out_end_ms)}
                    </span>
                    <Badge tone="bad">−{d.drop_points.toFixed(1)} pts</Badge>
                    {d.suggested && <span className="cr-muted cr-clamp-1">{d.suggested}</span>}
                    <span className="cr-muted retention-drop__map">
                      maps to {d.source_mappings.length} source range{d.source_mappings.length === 1 ? "" : "s"}
                    </span>
                  </span>
                }
              />
            ))}
            <div>
              <Button
                variant="primary"
                onClick={() => void apply()}
                disabled={(checked.size === 0 || active.stale) || (busy === "apply")}
                aria-busy={(busy === "apply") || undefined}
              >
                {busy === "apply" ? "Applying…" : <>Apply {checked.size} cut{checked.size === 1 ? "" : "s"}</>}
              </Button>
            </div>
          </div>
        )}

        {active && drops.length === 0 && !active.stale && (
          <p className="cr-muted retention-none">No drop-off cuts proposed for this import.</p>
        )}

        {imports.length === 0 && !error && (
          <EmptyState title="No retention data yet" icon={<Icon name="chart-line" width={28} height={28} />}>
            Export a render, then import the retention CSV your platform gives you.
          </EmptyState>
        )}
      </div>
    </PageShell>
  );
}
