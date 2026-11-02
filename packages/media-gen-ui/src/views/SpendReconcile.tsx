import { useEffect, useState } from 'react';
import { fetchCredentialStatus, reconcileSpend, type CredentialProvider, type SpendReconcileInput, type SpendReconcileResult } from '../lib/api.js';
import { useApp } from '../lib/app-state.js';
import { EngineError, errorCode, toError } from '../lib/engine-error.js';
import { Card, Badge, Button, Input, Notice, Select } from '@rightkit/react-shell/react';
import {
  alreadyRecordedNote,
  checkReconcileForm,
  conflictText,
  differenceRows,
  formatUsd,
  outcomeCounts,
  outcomeHeadline,
  unmatchedReasonText,
  utcMonthOf,
  type LineProblem,
  type PeriodChoice,
  type ReconcileForm
} from './spend-reconcile-logic.js';
import { hostAppName } from "../appName.js";

const NEVER_CHANGES = 'Matching never changes your spending limits, reserved budget, or what was counted against them. Nothing here spends money or contacts a provider.';

/** The engine's message without its code: codes are not shown as copy. */
function errorMessage(error: unknown): string {
  const e = toError(error);
  return e instanceof EngineError ? e.detail : e.message;
}

/** Settings > Spend: record a provider invoice beside the host app own amounts. Writes only the invoice record. */
export function SpendReconcile() {
  const app = useApp();
  const [providers, setProviders] = useState<CredentialProvider[] | null>(null);
  const [providersError, setProvidersError] = useState<string | null>(null);
  const [form, setForm] = useState<ReconcileForm>(() => ({
    provider: '',
    period: { mode: 'month', month: utcMonthOf() },
    total: '',
    invoiceRef: '',
    linesText: ''
  }));
  const [errors, setErrors] = useState<string[]>([]);
  const [lineProblems, setLineProblems] = useState<LineProblem[]>([]);
  const [pending, setPending] = useState<{ input: SpendReconcileInput; summary: string[] } | null>(null);
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<SpendReconcileResult | null>(null);
  const [failure, setFailure] = useState<string | null>(null);

  useEffect(() => {
    fetchCredentialStatus().then(
      (s) => setProviders(s.providers),
      (e) => setProvidersError(errorMessage(e))
    );
  }, []);

  /** Any edit drops the preview, so the confirm always matches what is on screen. */
  function edit(patch: Partial<ReconcileForm>) {
    setForm((f) => ({ ...f, ...patch }));
    setPending(null);
    setErrors([]);
    setLineProblems([]);
    setResult(null);
    setFailure(null);
  }

  function setPeriodMode(mode: 'month' | 'dates') {
    const period: PeriodChoice = mode === 'month'
      ? { mode: 'month', month: utcMonthOf() }
      : { mode: 'dates', from: '', to: '' };
    edit({ period });
  }

  function preview() {
    const checked = checkReconcileForm(form);
    if (!checked.ok) {
      setPending(null);
      setErrors(checked.errors);
      setLineProblems(checked.lineProblems);
      return;
    }
    setErrors([]);
    setLineProblems([]);
    setPending({ input: checked.input, summary: checked.summary });
  }

  async function confirm() {
    if (!pending || busy) return;
    setBusy(true);
    setFailure(null);
    try {
      const res = await reconcileSpend(pending.input);
      setResult(res);
      setPending(null);
      await app.refreshSpend();
    } catch (e) {
      const code = errorCode(e);
      setFailure(code === 'invoice_ref_conflict' ? conflictText(errorMessage(e)) : errorMessage(e));
      setPending(null);
    } finally {
      setBusy(false);
    }
  }

  const period = form.period;
  const counts = result ? outcomeCounts(result) : null;
  const note = result ? alreadyRecordedNote(result) : null;

  return (
    <div style={{ marginTop: 22 }}>
      <h3 style={{ margin: '0 0 2px', fontSize: 15, fontWeight: 600, letterSpacing: '-.01em' }}>Match a provider invoice</h3>
      <p style={{ margin: '4px 0 0', fontSize: 12.5, color: 'var(--ink-2)' }}>
        Record a provider`s invoice next to ${hostAppName()}`s own amounts. {NEVER_CHANGES}
      </p>

      {providersError ? <div style={{ marginTop: 12 }}><Notice tone="bad">{providersError}</Notice></div> : null}

      <Card flush className="group-box">
        <div className="setting">
          <span>Provider</span>
          {providers === null && !providersError ? <span style={{ color: 'var(--ink-3)' }}>Loading providers…</span> : null}
          {providers !== null ? (
            <Select label="Provider" value={form.provider} onChange={(v) => edit({ provider: v })} options={[{ value: '', label: 'Choose a provider' }, ...providers.map((p) => ({ value: p.id, label: p.id }))]} />
          ) : null}
        </div>
        <div className="setting">
          <span>Period (UTC)</span>
          <span style={{ display: 'flex', gap: 6, alignItems: 'center', flexWrap: 'wrap', justifyContent: 'flex-end' }}>
            <Select label="Period type" value={period.mode} onChange={(v) => setPeriodMode(v as 'month' | 'dates')} options={[{ value: 'month', label: 'Whole month' }, { value: 'dates', label: 'From and to dates' }]} />
            {period.mode === 'month' ? (
              <Input type="month" aria-label="Month (UTC)" value={period.month} onChange={(e) => edit({ period: { mode: 'month', month: e.target.value } })} />
            ) : (
              <>
                <Input type="date" aria-label="From date (UTC)" value={period.from} onChange={(e) => edit({ period: { mode: 'dates', from: e.target.value, to: period.to } })} />
                <Input type="date" aria-label="To date (UTC)" value={period.to} onChange={(e) => edit({ period: { mode: 'dates', from: period.from, to: e.target.value } })} />
              </>
            )}
          </span>
        </div>
        <div className="setting">
          <span>Invoice total</span>
          <span className="cap-input">
            <span style={{ color: 'var(--ink-3)' }}>$</span>
            <Input aria-label="Invoice total in dollars" inputMode="decimal" value={form.total} onChange={(e) => edit({ total: e.target.value })} />
          </span>
        </div>
        <div className="setting">
          <span>Invoice reference (optional)</span>
          <Input aria-label="Invoice reference" value={form.invoiceRef} onChange={(e) => edit({ invoiceRef: e.target.value })} placeholder="created for you if empty" />
        </div>
        <div className="setting" style={{ display: 'block' }}>
          <span style={{ display: 'block', marginBottom: 6 }}>
            Lines (optional), one per line: a {hostAppName()} job ID, a comma, then the amount in dollars, for example <code>abc123, 4.25</code>.
            To use the provider's job ID instead, put <code>provider:</code> before it. A third number after another comma picks the attempt (1 for the first try).
            Amounts are plain dollars such as 4.25, with no $ sign.
          </span>
          <textarea className="rk-input"
            aria-label="Invoice lines"
            rows={5}
            spellCheck={false}
            value={form.linesText}
            onChange={(e) => edit({ linesText: e.target.value })}
            style={{ width: '100%', boxSizing: 'border-box', font: 'inherit', fontSize: 12, fontFamily: 'var(--mono, ui-monospace, monospace)', background: 'var(--fill-2)', border: 0, borderRadius: 8, padding: 8, color: 'var(--ink)', resize: 'vertical' }}
          />
        </div>
      </Card>

      {errors.length > 0 ? (
        <div style={{ marginTop: 12 }}>
          <Notice tone="bad">
            {errors.map((m) => <div key={m}>{m}</div>)}
            {lineProblems.length > 0 ? (
              <ul style={{ margin: '6px 0 0', paddingLeft: 18 }}>
                {lineProblems.map((p) => <li key={p.lineNumber}>Line {p.lineNumber}: <code>{p.text}</code>. {p.reason}.</li>)}
              </ul>
            ) : null}
          </Notice>
        </div>
      ) : null}

      {pending ? (
        <div style={{ marginTop: 12 }}>
          <Notice tone="info">
            <div style={{ fontWeight: 600, color: 'var(--ink)', marginBottom: 6 }}>This will be recorded:</div>
            <ul style={{ margin: 0, paddingLeft: 18 }}>
              {pending.summary.map((s) => <li key={s} style={{ marginBottom: 3 }}>{s}</li>)}
            </ul>
            <div style={{ display: 'flex', gap: 8, marginTop: 10 }}>
              <Button type="button" variant="primary" disabled={busy} onClick={() => void confirm()}>Record this invoice</Button>
              <Button type="button" variant="secondary" disabled={busy} onClick={() => setPending(null)}>Edit</Button>
            </div>
          </Notice>
        </div>
      ) : (
        <div style={{ display: 'flex', justifyContent: 'flex-end', marginTop: 12 }}>
          <Button type="button" variant="secondary" disabled={providers === null} onClick={preview}>Preview</Button>
        </div>
      )}

      {failure ? <div style={{ marginTop: 12 }}><Notice tone="bad">{failure}</Notice></div> : null}

      {result ? (
        <Card flush className="group-box gb-mt12">
          <div className="setting" style={{ display: 'block' }}>
            <div style={{ fontWeight: 600 }}>{outcomeHeadline(result)}</div>
            {note ? <div style={{ color: 'var(--ink-2)', marginTop: 4 }}>{note}</div> : null}
            <div style={{ color: 'var(--ink-3)', marginTop: 4 }}>Invoice reference {result.invoice_ref}{result.invoice_ref_generated ? ' (created for you)' : ''}. Invoice total {formatUsd(result.invoice_total_usd)}.</div>
          </div>
          {result.lines && counts ? (
            <>
              <div className="setting"><span>Lines matched</span><span style={{ color: 'var(--ink-2)' }}>{counts.matched} of {result.lines.given}</span></div>
              <div className="setting"><span>Lines with no matching record</span><span style={{ color: 'var(--ink-2)' }}>{counts.noRow}</span></div>
              {counts.otherUnmatched > 0 ? <div className="setting"><span>Other lines not matched</span><span style={{ color: 'var(--ink-2)' }}>{counts.otherUnmatched}</span></div> : null}
              <div className="setting"><span>Records with no line</span><span style={{ color: 'var(--ink-2)' }}>{counts.rowsWithoutLine}</span></div>
              {result.lines.matched.map((m) => (
                <div className="setting" key={`${m.job_id}:${m.attempt}`}>
                  <span>Matched {m.job_id} (attempt {m.attempt})</span>
                  <span style={{ color: 'var(--ink-2)' }}>invoice {formatUsd(m.invoice_usd)}, settled {formatUsd(m.settled_usd)}</span>
                </div>
              ))}
              {result.lines.unmatched_lines.map((u) => (
                <div className="setting" key={`u${u.index}`}>
                  <span>Not matched: {u.job_id ? `job ${u.job_id}` : `provider job ${u.provider_job_id}`}{u.attempt !== null ? ` (attempt ${u.attempt})` : ''}, {formatUsd(u.usd)}</span>
                  <span style={{ color: 'var(--ink-2)', textAlign: 'right' }}>{unmatchedReasonText(u.reason)}</span>
                </div>
              ))}
              {result.lines.unmatched_rows.map((r) => (
                <div className="setting" key={`r:${r.job_id}:${r.attempt}`}>
                  <span>No line for {r.job_id} (attempt {r.attempt})</span>
                  <span style={{ color: 'var(--ink-2)' }}>settled {r.settled_usd === null ? 'not yet' : formatUsd(r.settled_usd)}, reserved {formatUsd(r.reserved_usd)}</span>
                </div>
              ))}
            </>
          ) : null}
          <div className="setting" style={{ display: 'block' }}>
            <div style={{ fontWeight: 600, marginBottom: 2 }}>Differences</div>
            {differenceRows(result.differences).map((d) => (
              <div key={d.label} style={{ display: 'flex', justifyContent: 'space-between', gap: 10, marginTop: 3 }}>
                <span>{d.label}</span><span style={{ color: 'var(--ink-2)' }}>{d.value}</span>
              </div>
            ))}
          </div>
          <div className="setting"><span>Records matched to this invoice</span><span style={{ color: 'var(--ink-2)' }}>{result.invoice_reconciled_rows} {result.invoice_reconciled_rows === 1 ? 'record' : 'records'}{result.invoice_reconciled_usd === null ? '' : `, ${formatUsd(result.invoice_reconciled_usd)}`}</span></div>
          <div className="setting"><span>Spending limits and counted amounts</span><Badge>unchanged by this invoice</Badge></div>
        </Card>
      ) : null}
    </div>
  );
}
