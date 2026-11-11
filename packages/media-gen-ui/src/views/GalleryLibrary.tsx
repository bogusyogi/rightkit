// Library search results and the asset import panel, shown from the Gallery view.
import { useState, type FormEvent, type ReactNode } from 'react';
import { importAsset, previewAssetImport } from '../lib/api.js';
import { EngineError, toError } from '../lib/engine-error.js';
import { Badge, Button, Card, EmptyState, Input, Notice, Select } from '@rightkit/react-shell/react';
import { Kv, baseName } from '../components/ui.js';
import { evidenceLabel, factsRows, notIndexedText, originKindLabel, parseSnippet, rightsLabel, scopeLabel } from '../lib/asset-facts.js';
import { EMPTY_IMPORT_FORM, IMPORT_EVIDENCE_OPTIONS, IMPORT_ORIGIN_OPTIONS, IMPORT_RIGHTS_OPTIONS, validateImportForm, type ImportForm, type ImportFormErrors } from '../lib/asset-import.js';
import type { AssetImportDryRun, AssetImportResult, ImportEvidence, ImportRightsStatus, LibraryHit } from '../lib/types/library.js';

/** The engine's message without its code: codes are not shown as copy. */
function errorText(error: unknown): string {
  const e = toError(error);
  return e instanceof EngineError ? e.detail : e.message;
}

interface LibraryResultsProps {
  query: string;
  hits: LibraryHit[];
  total: number;
  hasMore: boolean;
  notIndexed: string[];
  rebuilt: boolean;
  busy: boolean;
  loadingMore: boolean;
  error: string | null;
  selectedId: string | null;
  onSelect: (hit: LibraryHit) => void;
  onLoadMore: () => void;
  onBack: () => void;
}

function LibraryHitRow({ hit, selected, onSelect }: { hit: LibraryHit; selected: boolean; onSelect: (hit: LibraryHit) => void }) {
  const parts = parseSnippet(hit.prompt_snippet);
  const kindLine = [hit.kind, hit.provider, hit.model].filter(Boolean).join(' · ');
  return (
    <Card className="lib-hit" selected={selected} label={baseName(hit.path, hit.asset_id)} onClick={() => onSelect(hit)}>
      <span className="lib-hit-head">
        <b>{baseName(hit.path, hit.asset_id)}</b>
        <span>{kindLine}</span>
      </span>
      <span className="lib-snippet">
        {parts.length
          ? parts.map((part, i) => (part.match ? <mark key={i}>{part.text}</mark> : <span key={i}>{part.text}</span>))
          : 'No prompt text recorded.'}
      </span>
      <span className="lib-chips">
        <Badge>Origin: {originKindLabel(hit.facts?.origin)}</Badge>
        <Badge>Evidence: {evidenceLabel(hit.facts?.evidence)}</Badge>
        <Badge>Rights: {rightsLabel(hit.facts?.rights?.status)}</Badge>
        {!hit.exists ? <Badge tone="warn">File missing from disk</Badge> : null}
      </span>
      {hit.scope ? <span className="lib-scope">Scope: {scopeLabel(hit.scope)}</span> : null}
    </Card>
  );
}

export function LibraryResults(p: LibraryResultsProps) {
  return (
    <div className="lib-results" aria-live="polite">
      <div className="lib-head">
        <span>{p.error ? '' : p.busy && p.hits.length === 0 ? 'Searching…' : `${p.total} ${p.total === 1 ? 'result' : 'results'} for "${p.query}"`}</span>
        <Button type="button" variant="ghost" size="sm" className="gal-clear" onClick={p.onBack}>Back to gallery</Button>
      </div>
      {p.notIndexed.length ? (
        <p className="lib-note">
          Not searched yet: {notIndexedText(p.notIndexed)}.{p.rebuilt ? ' The search index was rebuilt.' : ''}
        </p>
      ) : null}
      {p.error ? <Notice tone="bad" className="lib-err">{p.error}</Notice> : null}
      {!p.busy && !p.error && p.hits.length === 0 ? (
        <EmptyState title="Nothing matches this search.">
          Check the spelling, or remove a filter word such as kind: or rights:.
        </EmptyState>
      ) : null}
      <ul className="lib-hits">
        {p.hits.map((hit) => (
          <li key={hit.asset_id}>
            <LibraryHitRow hit={hit} selected={p.selectedId === `lib:${hit.asset_id}`} onSelect={p.onSelect} />
          </li>
        ))}
      </ul>
      {p.hasMore ? (
        <div className="gal-more">
          <Button type="button" variant="ghost" size="sm" className="gal-clear" disabled={p.loadingMore} onClick={p.onLoadMore}>
            {p.loadingMore ? 'Loading…' : 'Load more'}
          </Button>
        </div>
      ) : null}
    </div>
  );
}

function Field({ label, wide, error, children }: { label: string; wide?: boolean; error?: string; children: ReactNode }) {
  return (
    <label className={`imp-field${wide ? ' wide' : ''}`}>
      <span>{label}</span>
      {children}
      {error ? <Notice tone="bad">{error}</Notice> : null}
    </label>
  );
}

function ImportPreview({ dryRun }: { dryRun: AssetImportDryRun }) {
  const w = dryRun.would_import;
  return (
    <div className="imp-preview" aria-label="What the import will record">
      <h4>Would record (nothing is written yet)</h4>
      <Kv
        rows={[
          ['Source', w.path],
          ['File type', w.mime ?? 'not recorded'],
          ['File size', w.bytes != null ? `${w.bytes.toLocaleString()} bytes` : 'not recorded'],
          ...factsRows(w.facts, w.scope)
        ]}
      />
    </div>
  );
}

interface ImportPanelProps {
  onClose: () => void;
  onImported: (result: AssetImportResult) => void;
}

/** Import form: validates here, then the engine's dry run decides what is recorded. Import needs a preview first. */
export function ImportPanel({ onClose, onImported }: ImportPanelProps) {
  const [form, setForm] = useState<ImportForm>(EMPTY_IMPORT_FORM);
  const [errors, setErrors] = useState<ImportFormErrors>({});
  const [preview, setPreview] = useState<AssetImportDryRun | null>(null);
  const [previewBusy, setPreviewBusy] = useState(false);
  const [importBusy, setImportBusy] = useState(false);
  const [result, setResult] = useState<AssetImportResult | null>(null);
  const [error, setError] = useState<string | null>(null);

  // Any edit makes an earlier preview stale, so Import waits for a new one.
  function change(patch: Partial<ImportForm>) {
    setForm((f) => ({ ...f, ...patch }));
    setPreview(null);
    setResult(null);
    setError(null);
  }

  async function runPreview(event: FormEvent) {
    event.preventDefault();
    const checked = validateImportForm(form);
    setErrors(checked.errors);
    setError(null);
    if (!checked.request) return;
    setPreviewBusy(true);
    try {
      setPreview(await previewAssetImport(checked.request));
    } catch (e) {
      setError(errorText(e));
    } finally {
      setPreviewBusy(false);
    }
  }

  async function runImport() {
    const checked = validateImportForm(form);
    setErrors(checked.errors);
    if (!checked.request || !preview) return;
    setImportBusy(true);
    setError(null);
    try {
      const done = await importAsset(checked.request);
      setResult(done);
      setPreview(null);
      onImported(done);
    } catch (e) {
      setError(errorText(e));
    } finally {
      setImportBusy(false);
    }
  }

  const busy = previewBusy || importBusy;
  return (
    <form className="imp-panel" aria-label="Import file" onSubmit={(e) => void runPreview(e)} noValidate>
      <div className="imp-head">
        <h3>Import file</h3>
        <Button type="button" variant="ghost" size="sm" className="gal-clear" onClick={onClose}>Close</Button>
      </div>
      <p className="imp-hint">Copies a file from outside the library into it, with its origin, evidence, rights and scope. The original stays where it is.</p>
      <div className="imp-grid">
        <Field label="File path" wide error={errors.path}>
          <Input value={form.path} placeholder="/Users/you/Pictures/photo.png" onChange={(e) => change({ path: e.target.value })} aria-invalid={Boolean(errors.path)} />
        </Field>
        <Field label="Origin" error={errors.origin}>
          <select className="rk-select" value={form.origin} onChange={(e) => change({ origin: e.target.value as ImportForm['origin'] })} aria-invalid={Boolean(errors.origin)}>
            <option value="">Choose…</option>
            {IMPORT_ORIGIN_OPTIONS.map((o) => (
              <option key={o.value} value={o.value}>{o.label}</option>
            ))}
          </select>
        </Field>
        <Field label="Evidence">
          <Select label="Evidence" value={form.evidence} onChange={(v) => change({ evidence: v as ImportEvidence })} options={IMPORT_EVIDENCE_OPTIONS} />
        </Field>
        <Field label="Rights">
          <Select label="Rights" value={form.rightsStatus} onChange={(v) => change({ rightsStatus: v as ImportRightsStatus })} options={IMPORT_RIGHTS_OPTIONS} />
        </Field>
        <Field label="Attribution">
          <Input value={form.attribution} placeholder="Optional" onChange={(e) => change({ attribution: e.target.value })} />
        </Field>
        <Field label="Licence evidence" wide error={errors.licence}>
          <textarea className="rk-input" rows={2} value={form.licenceEvidence} placeholder="One per line: a receipt, licence id or URL. Required for cleared rights." onChange={(e) => change({ licenceEvidence: e.target.value })} aria-invalid={Boolean(errors.licence)} />
        </Field>
        <Field label="Venture">
          <Input value={form.venture} placeholder="e.g. acme" onChange={(e) => change({ venture: e.target.value })} />
        </Field>
        <Field label="Channel">
          <Input value={form.channel} placeholder="e.g. cutright-how-to" onChange={(e) => change({ channel: e.target.value })} />
        </Field>
        <Field label="Account">
          <Input value={form.account} placeholder="e.g. main" onChange={(e) => change({ account: e.target.value })} />
        </Field>
      </div>
      {errors.scope ? <Notice tone="bad">{errors.scope}</Notice> : null}
      <p className="imp-hint">Scope is optional: fill in venture, channel and account together, or leave all three empty.</p>
      <div className="imp-actions">
        <Button type="submit" disabled={busy}>{previewBusy ? 'Checking…' : 'Preview import'}</Button>
        <Button type="button" variant="primary" disabled={!preview || busy} onClick={() => void runImport()}>{importBusy ? 'Importing…' : 'Import'}</Button>
      </div>
      {error ? <Notice tone="bad">{error}</Notice> : null}
      {preview ? <ImportPreview dryRun={preview} /> : null}
      {result ? (
        <Notice tone="ok" className="imp-done">
          Imported. The file was copied into the library.{result.source_preserved ? ' The original is unchanged.' : ''}
          <span className="mono" style={{ display: 'block', fontSize: 10.5, color: 'var(--ink-3)' }}>ID {result.asset_id}</span>
        </Notice>
      ) : null}
    </form>
  );
}
