// Exports — docs/design/app-ui.md §3.5.
import { Badge, Button, Checkbox, ContextMenu, EmptyState, Field, IconButton, Input, PageShell, Popover, Select, useToast } from "@rightkit/react-shell/react";
import { useRef, useState } from "react";
import type { PackagePlatform, QaReportRow, RenderFile } from "../../lib/media.js";
import { MediaPlayer } from "@rightkit/media-ui";
import { useVideoExportPackage, useVideoExports, useVideoExportThumbnail, type VideoExportPreset } from "../hooks/useVideoExports.js";
import { Drawer, ErrorPanel, Icon, ProgressRow, Thumbnail, fileSize, timecode } from "../ui/index.js";
import type { ProgressState } from "../hooks/useProgress.js";
import { BatchDelivery } from "./BatchDelivery.js";
import { BundleExport } from "./BundleExport.js";

type Format = {
  preset: VideoExportPreset;
  title: string;
  sub: string;
  aspect: string;
  vertical: boolean;
  supported: boolean;
};

const FORMATS: Format[] = [
  { preset: "youtube", title: "YouTube 16:9", sub: "1080p", aspect: "16 / 9", vertical: false, supported: true },
  {
    preset: "reels",
    title: "Shorts / Reels / TikTok",
    sub: "9:16",
    aspect: "9 / 16",
    vertical: true,
    supported: true,
  },
  { preset: "square", title: "Square", sub: "1:1", aspect: "1 / 1", vertical: true, supported: true },
];

const PRESET_ASPECT: Record<string, string> = { youtube: "16 / 9", reels: "9 / 16", tiktok: "9 / 16", square: "1 / 1" };

function RenderThumb({ dir, render, aspect }: { dir: string; render: string; aspect: string }) {
  const src = useVideoExportThumbnail(dir, render);
  return <Thumbnail src={src} aspect={aspect} />;
}

/** ⋯ menu per export card. */
function CardMenu({
  onCopy,
  onDelete,
  onImportRetention,
  deleteBlocked,
}: {
  onCopy: () => void;
  onDelete: () => void;
  deleteBlocked: boolean;
  onImportRetention?: () => void;
}) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLButtonElement | null>(null);
  return (
    <div className="exports-menu">
      <IconButton ref={ref} label="More actions" onClick={() => setOpen((v) => !v)}><Icon name="more-horizontal" /></IconButton>
      {open && ref.current && (
        <ContextMenu
          anchor={ref.current}
          placement="top-end"
          label="Export actions"
          width={160}
          className="exports-menu__pop"
          itemAs="button"
          keyboard="escape-only"
          autoFocus={false}
          onClose={() => setOpen(false)}
          items={[
            { id: "copy", label: "Copy path", icon: <Icon name="copy" />, run: () => { setOpen(false); onCopy(); } },
            { id: "retention", label: "Import retention…", icon: <Icon name="chart-line" />, disabled: !onImportRetention, run: () => { setOpen(false); onImportRetention?.(); } },
            { id: "delete", label: "Delete export", disabled: deleteBlocked, icon: <Icon name="trash-2" />, danger: true, run: () => { setOpen(false); onDelete(); } },
          ]}
          renderItem={(item) => (
            <span title={item.id === "retention" && item.disabled ? "Retention screen routing pending" : undefined}>
              {item.icon} {item.label}
            </span>
          )}
        />
      )}
    </div>
  );
}

/** QA chip + issues popover (§3.5). */
function QaChip({ report, running, blocked, onRun }: { report?: QaReportRow; running: boolean; blocked: boolean; onRun: () => void }) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLButtonElement | null>(null);
  if (!report) {
    return (
      <Button variant="ghost" size="sm" className="cr-chip-btn" onClick={onRun} disabled={blocked} aria-busy={running || undefined} title="Run QA checks on this export"><Badge tone="neutral">
        {running ? "Checking…" : "Run QA"}
      </Badge></Button>
    );
  }
  const ok = report.status === "pass" && report.issues === 0;
  const failed = (report.checks ?? []).filter((c) => c.status !== "pass");
  return (
    <div className="exports-menu">
      <Button variant="ghost" size="sm" ref={ref} className="cr-chip-btn" onClick={() => setOpen((v) => !v)}><Badge tone={ok ? "ok" : "warn"}>
        {ok ? "QA passed" : `${report.issues} issues`}
      </Badge></Button>
      {open && (
        <Popover anchor={ref.current} placement="top-start" width={280} label="Export QA checks" className="exports-qa__pop" onClose={() => setOpen(false)}>
          {failed.length === 0 ? (
            <p className="cr-muted">All checks passed.</p>
          ) : (
            <ul>
              {failed.map((c, i) => (
                <li key={c.id ?? i}>
                  <span className="cr-mono cr-muted">{c.id}</span>
                  {c.detail ? ` — ${c.detail}` : ""}
                </li>
              ))}
            </ul>
          )}
          <Button size="sm" variant="ghost" onClick={onRun} disabled={blocked} aria-busy={running || undefined}>
            {running ? "Running QA…" : "Re-run QA"}
          </Button>
        </Popover>
      )}
    </div>
  );
}

const PACKAGE_PLATFORMS: { value: PackagePlatform; label: string }[] = [
  { value: "youtube", label: "YouTube" },
  { value: "shorts", label: "YouTube Shorts" },
  { value: "reels", label: "Reels" },
  { value: "tiktok", label: "TikTok" },
];

function PackageForm({ dir, render, onClose }: { dir: string; render: RenderFile; onClose: () => void }) {
  const { platform, setPlatform, title, setTitle, description, setDescription, tags, setTags, folder, error, busy, canSubmit, submit, reveal } = useVideoExportPackage(dir, render);

  return (
    <form className="exports-package" onSubmit={(event) => { event.preventDefault(); void submit(); }}>
      <div className="exports-package__head">
        <span className="cr-overline">Package</span>
        <IconButton label="Close package form" onClick={onClose}><Icon name="x" /></IconButton>
      </div>
      <Field label="Platform">{() => (<Select<PackagePlatform>
          label="Platform"
          value={platform}
          onChange={setPlatform}
          options={PACKAGE_PLATFORMS}
        />)}</Field>
      <Field label="Title">{(ids) => (<Input {...ids} value={title} onChange={(e) => setTitle(e.target.value)} required />)}</Field>
      <Field label="Description">{(ids) => (<Input {...ids} value={description} onChange={(e) => setDescription(e.target.value)} />)}</Field>
      <Field label="Tags" note={"Separate with commas"}>{(ids) => (<Input {...ids} value={tags} onChange={(e) => setTags(e.target.value)} />)}</Field>
      <div className="exports-package__actions">
        <Button type="submit" variant="primary" size="sm" disabled={!canSubmit} aria-busy={busy || undefined}>
          {busy ? "Packaging…" : "Build package"}
        </Button>
        {folder && (
          <Button size="sm" variant="ghost" onClick={() => void reveal(folder)}>
            Reveal package
          </Button>
        )}
      </div>
      {folder && (
        <p role="status" className="exports-package__result cr-muted">
          Packaged to <span className="cr-mono">{folder}</span>
        </p>
      )}
      {error && (
        <p role="alert" className="exports-package__error">
          {error.kind === "qa_failed" ? "This render has not passed QA — " : error.kind === "qa_missing" ? "This render has not been checked yet; run QA first — " : ""}
          {error.detail}
        </p>
      )}
    </form>
  );
}

type ExportsViewProps = {
  dir: string;
  progress?: ProgressState;
  rendering?: boolean;
  outputPath?: string | null;
  onRender?: (preset: VideoExportPreset) => void;
  onChanged: () => Promise<void>;
  refreshChangedProject?: (dir: string) => Promise<void>;
  captureSelection?: () => () => boolean;
  onImportRetention?: (render: string) => void;
};

const framingLabels = {
  approved_automatically: "Approved automatically",
  approved: "Framing approved",
  automatic: "Framing: automatic",
  needs_approval: "Needs framing approval",
};

export function ExportsView(props: ExportsViewProps) {
  return <ProjectExportsView key={props.dir} {...props} />;
}

function ProjectExportsView({
  dir, progress, rendering, onChanged, refreshChangedProject, captureSelection, onImportRetention,
}: ExportsViewProps) {
  const toast = useToast();
  const [packaging, setPackaging] = useState<string | null>(null);
  const {
    renders, mp4s, documents, loadError, hasSources, qaFor, preview, previewRender, closePreview,
    drawerOpen, openDrawer, closeDrawer, picked, setPreset, framingStatus, interchange, setInterchange,
    canStart, jobs, active, target, showQueue, queueError, interchangeFailures,
    startExport, cancelJob, dismissJob, runQa, deleteRender, reveal, copyPath,
  } = useVideoExports({
    dir, progressDone: progress?.done, rendering, onChanged: refreshChangedProject ?? onChanged, captureSelection,
    onFeedback: (feedback) => {
      if (feedback.kind === "deleted") toast("Deleted " + feedback.name, { kind: "success" });
      else toast(feedback.detail, { kind: "error", ttl: 0 });
    },
  });

  return (
    <PageShell
      title="Exports"
      actions={
        <Button variant="primary" onClick={openDrawer} disabled={!hasSources}><Icon name="download" />
          Export…
        </Button>
      }
      maxWidth={0}
      subtitle={renders
          ? `${mp4s.length} export${mp4s.length === 1 ? "" : "s"}`
          : undefined}
    >
      <div className="exports-body">
        {loadError && <ErrorPanel title="Exports failed to load" detail={loadError} />}
        {queueError && <ErrorPanel title="Export queue" detail={queueError} />}
        {interchangeFailures.map(({ format, detail }) => (
          <ErrorPanel key={format} title={format.toUpperCase() + " export failed"} detail={detail} />
        ))}

        {showQueue && (
          <div className="exports-queue">
            <span className="cr-overline">Queue</span>
            {jobs.map((j) => (
              <div key={j.job} className="exports-queue__row">
                <ProgressRow
                  label={`Render ${FORMATS.find(({ preset }) => preset === j.preset)?.title ?? j.preset}`}
                  status={
                    j.status === "running"
                      ? "Rendering"
                      : j.status === "queued"
                        ? "Queued"
                        : j.status === "done"
                          ? "Done"
                          : j.status === "cancelled"
                            ? "Cancelled"
                            : "Failed"
                  }
                  tone={j.status === "failed" ? "danger" : j.status === "done" ? "ok" : "info"}
                  fraction={j.status === "done" ? 1 : j.status === "failed" || j.status === "cancelled" ? null : j.fraction}
                  error={j.error ?? j.cancelError ?? (j.cancelDeclined ? "Cancellation was not accepted" : undefined)}
                />
                {(j.status === "queued" || j.status === "running") && (
                  <Button size="sm" variant="ghost" onClick={() => cancelJob(j.job)} disabled={j.cancelling}>
                    {j.cancelling ? "Cancelling…" : "Cancel"}
                  </Button>
                )}
                {(j.status === "failed" || j.status === "cancelled" || j.status === "done") && (
                  <IconButton
                    label="Dismiss"
                    onClick={() => dismissJob(j.job)}
                  ><Icon name="x" /></IconButton>
                )}
              </div>
            ))}
          </div>
        )}

        {preview && (
          <div className="exports-preview">
            <div className="exports-preview__head">
              <span className="cr-overline">Playing</span>
              <IconButton label="Close preview" onClick={closePreview}><Icon name="x" /></IconButton>
            </div>
            <MediaPlayer
              key={preview.playbackId ?? "pending"}
              src={preview.path}
              playbackId={preview.playbackId}
              kind={preview.kind}
              title={preview.path.split(/[\\/]/).pop()}
              autoPlay
            />
          </div>
        )}

        {renders === undefined ? (
          <ProgressRow label="Loading exports" status="Loading" fraction={undefined} />
        ) : mp4s.length === 0 && documents.length === 0 ? (
          <EmptyState
            title="No exports yet"
            action={<Button variant="primary" onClick={openDrawer}>Export…</Button>}
            icon={<Icon name="download" width={28} height={28} />}
          >
            Render your finished edit to share it.
          </EmptyState>
        ) : (
          <div className="exports-grid">
            {mp4s.map((r) => {
              const rec = r.record;
              const aspect = PRESET_ASPECT[rec?.preset ?? ""] ?? "16 / 9";
              const qa = qaFor(r);
              const lufs = rec?.loudness?.after?.input_i ?? rec?.loudness?.measured?.input_i;
              const meta = [
                rec?.duration_ms != null ? timecode(rec.duration_ms) : null,
                rec?.preset === "reels" || rec?.preset === "tiktok" ? "9:16" : "16:9",
                fileSize(r.bytes),
                rec?.origin?.timeline_cursor != null ? `rev ${rec.origin.timeline_cursor}` : null,
              ]
                .filter(Boolean)
                .join(" · ");
              return (
                <div key={r.path} className="exports-card">
                  <button
                    type="button"
                    className="exports-card__thumb"
                    aria-label={`Play ${r.name}`}
                    onClick={() => previewRender(r)}
                  >
                    <RenderThumb dir={dir} render={r.name} aspect={aspect} />
                  </button>
                  <div className="exports-card__name cr-clamp-1" title={r.name}>
                    {r.name}
                  </div>
                  <div className="exports-card__meta cr-muted">{meta}</div>
                  <div className="exports-card__row">
                    <QaChip report={qa} running={active === "qa" && target === r.name} blocked={!!active} onRun={() => void runQa(r)} />
                    <Button
                      size="sm"
                      variant="ghost"
                      aria-label={`Package ${r.name}`}
                      aria-expanded={packaging === r.name}
                      onClick={() => setPackaging((n) => (n === r.name ? null : r.name))}
                    >
                      Package
                    </Button>
                    {lufs != null && Number.isFinite(lufs) && (
                      <span className="exports-lufs">{lufs.toFixed(1)} LUFS</span>
                    )}
                    <span className="exports-card__spacer" />
                    <Button size="sm" variant="ghost" onClick={() => void reveal(r.path)}>
                      Reveal
                    </Button>
                    <CardMenu
                      onCopy={() => void copyPath(r.path).then(() => toast("Path copied", { kind: "success" }), () => toast("Could not copy path", { kind: "error", ttl: 0 }))}
                      onDelete={() => void deleteRender(r)}
                      deleteBlocked={!!active}
                      onImportRetention={
                        onImportRetention ? () => onImportRetention(r.name) : undefined
                      }
                    />
                  </div>
                  {packaging === r.name && (
                    <PackageForm dir={dir} render={r} onClose={() => setPackaging(null)} />
                  )}
                </div>
              );
            })}
          </div>
        )}

        {documents.length > 0 && (
          <div className="exports-docs">
            <span className="cr-overline">Files</span>
            {documents.map((d) => (
              <div key={d.path} className="exports-doc">
                <Icon name="captions" />
                <span className="cr-clamp-1" title={d.path}>
                  {d.name}
                </span>
                <span className="cr-mono cr-muted">{fileSize(d.bytes)}</span>
                <IconButton label="Reveal" onClick={() => void reveal(d.path)}><Icon name="folder-open" /></IconButton>
              </div>
            ))}
          </div>
        )}

        <BatchDelivery dir={dir} />
        <BundleExport dir={dir} />
      </div>

      <Drawer
        title="Export"
        open={drawerOpen}
        onClose={closeDrawer}
        footer={
          <Button
            variant="primary"
            disabled={!canStart}
            onClick={() => void startExport()}
          >
            Export {picked.size > 0 ? picked.size : ""}
          </Button>
        }
      >
        <span className="cr-overline">Formats</span>
        {FORMATS.map((f) => {
          const on = picked.has(f.preset);
          const framing = framingStatus(f.preset);
          return (
            <Checkbox
              key={f.preset}
              className={`exports-format ${f.supported ? "" : "exports-format--off"}`}
              checked={on && f.supported}
              disabled={!f.supported}
              onChange={(checked) => setPreset(f.preset, checked)}
              label={<span className="exports-format__body">
                <span className="exports-format__title">
                  {f.title}
                  <span className="cr-muted"> {f.sub}</span>
                </span>
                {!f.supported && (
                  <span title={"Square output is not supported by the render engine yet"}><Badge tone="warn">
                    Not available
                  </Badge></span>
                )}
                {f.supported && f.vertical && (
                  <span title={framing === "automatic"
                        ? "Saved policy approves analysed framing during export"
                        : framing === "needs_approval"
                        ? "Vertical exports need an approved reframe plan per source"
                        : "Framing approved for all sources"}><Badge
                    tone={framing === "needs_approval" ? "warn" : "ok"}
                  >
                    {framingLabels[framing]}
                  </Badge></span>
                )}
              </span>}
            />
          );
        })}
        <Checkbox
          className="exports-format"
          checked={interchange}
          onChange={setInterchange}
          label={<span className="exports-format__body">
            <span className="exports-format__title">Also export for Resolve / Premiere</span>
            <span className="cr-muted">FCPXML, OTIO and captions next to the renders</span>
          </span>}
        />
      </Drawer>
    </PageShell>
  );
}
