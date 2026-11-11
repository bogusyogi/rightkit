import { useCallback, useEffect, useState } from 'react';
import { rentalResolve, rentalTeardown, rentalsEnforce, rentalsList, studio } from '../lib/api.js';
import { errorCode } from '../lib/engine-error.js';
import { useConfirm } from '../lib/confirm.js';
import { Card, Badge, Button, Notice } from '@rightkit/react-shell/react';
import {
  RENTAL_ENFORCEMENT_TEXT,
  RENTAL_LEAD,
  RENTAL_SUMMARY,
  destroyNeedsForce,
  enforceLines,
  limitRows,
  plainEngineText,
  plainErrorText,
  reasonText,
  rentalActions,
  rentalFigures,
  rentalStatusLabel,
  rentalStatusTone,
  rentalWarnings,
  settlementText,
  usd,
  workerStateText,
  formatDuration,
  type RentalListEntry,
  type RentalRecord,
  type RentalsEnforceReport,
  type RentalsListing,
  type RentalWorkerRecord,
} from './rentals-logic.js';
import { hostAppName } from "../appName.js";

const POLL_MS = 30_000;

const badgeTone = (t?: 'ok' | 'bad' | 'warn' | 'run' | 'local'): 'neutral' | 'accent' | 'ok' | 'warn' | 'bad' => (t === 'run' ? 'accent' : t === 'local' ? 'ok' : t ?? 'neutral');


/**
 * Rented workers: every Vast rented worker from `studio.models.rentals`, with status, elapsed, accrued
 * cost, the three limits, settlement and every warning. It never rents: renting needs the user's explicit
 * authorisation in the Provision form of the Rented workers section below. Destroy, find at provider and
 * the limit check each ask first.
 */
export function RentalsPanel() {
  const [listing, setListing] = useState<RentalsListing | null>(null);
  const [records, setRecords] = useState<Record<string, RentalRecord>>({});
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [report, setReport] = useState<RentalsEnforceReport | null>(null);
  const [noneRefused, setNoneRefused] = useState<Record<string, true>>({});
  const confirm = useConfirm();

  const refresh = useCallback(async () => {
    try {
      const [list, ws] = await Promise.all([
        rentalsList(),
        studio<{ workers: RentalWorkerRecord[] }>('models.workers'),
      ]);
      setListing(list);
      const byId: Record<string, RentalRecord> = {};
      for (const w of ws.workers ?? []) {
        if (w.rental) byId[w.worker_id] = w.rental;
      }
      setRecords(byId);
      setError(null);
    } catch (e) {
      setError(plainErrorText(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void refresh();
    const timer = setInterval(() => void refresh(), POLL_MS);
    return () => clearInterval(timer);
  }, [refresh]);

  const runAction = async (key: string, fn: () => Promise<void>) => {
    setBusy(key);
    setNotice(null);
    setError(null);
    try {
      await fn();
    } catch (e) {
      setError(plainErrorText(e));
    } finally {
      await refresh();
      setBusy(null);
    }
  };

  const destroy = async (e: RentalListEntry) => {
    const force = destroyNeedsForce(e.worker_state);
    const ok = await confirm({
      title: 'Destroy this rented worker now?',
      paragraphs: [
        `The computer at Vast.ai is destroyed, and its charge is settled for the time it ran, at its hourly price. If Vast.ai does not confirm the destroy, ${hostAppName()} records that the worker may still be charging.`,
        ...(force ? ['This worker is running a job. Destroying it now interrupts that job; the destroy is forced.'] : []),
        'Destroying from this screen needs your Vast.ai API key. Without it, nothing is destroyed.',
      ],
      subject: e.label ?? e.worker_id,
      confirmLabel: 'Destroy now',
      danger: true,
    });
    if (!ok) return;
    await runAction(`destroy:${e.worker_id}`, async () => {
      const w = await rentalTeardown(e.worker_id, force);
      const settled = settlementText(w.rental);
      setNotice(`Destroyed ${e.label ?? e.worker_id}${settled ? `: ${settled}` : '.'}`);
    });
  };

  const resolve = async (e: RentalListEntry) => {
    const ok = await confirm({
      title: 'Find this worker at Vast.ai?',
      paragraphs: [
        `Vast.ai may have started a computer for this worker. ${hostAppName()} looks for Vast.ai computers labelled ${e.label ?? e.worker_id}, using your Vast.ai API key, and changes nothing else.`,
        `One computer found: ${hostAppName()} takes it over. The worker becomes active again and its limits apply until it is destroyed.`,
        'Several found: nothing changes. Destroy the extra computers on Vast.ai first, then look again.',
        'None found: nothing changes. You will be asked separately before the reserved budget is released.',
      ],
      subject: e.label ?? e.worker_id,
      confirmLabel: 'Look for it',
    });
    if (!ok) return;
    await runAction(`resolve:${e.worker_id}`, async () => {
      try {
        const w = await rentalResolve(e.worker_id, false);
        setNoneRefused((prev) => {
          const next = { ...prev };
          delete next[e.worker_id];
          return next;
        });
        setNotice(`Took over Vast.ai computer ${String(w.rental?.instance_id ?? '')}: the worker is now ${rentalStatusLabel(w.rental?.status ?? 'resolved').toLowerCase()} and its limits apply.`);
      } catch (err) {
        if (errorCode(err) !== 'rental_unresolved') throw err;
        // Nothing changed. The lookup found no single computer; the user may confirm that none exists.
        setNoneRefused((prev) => ({ ...prev, [e.worker_id]: true }));
        setError(`Search result: ${plainErrorText(err)}`);
      }
    });
  };

  const releaseNone = async (e: RentalListEntry) => {
    const ok = await confirm({
      title: 'Release the reserved budget?',
      paragraphs: [
        `Confirm that no Vast.ai computer exists labelled ${e.label ?? e.worker_id}. ${hostAppName()} releases the reserved budget only when its own search also finds none; if it finds one, nothing changes.`,
        'After release the worker is no longer tracked, and its reserved budget goes back to the daily cap, the monthly cap and this device\'s allowance.',
      ],
      subject: e.label ?? e.worker_id,
      confirmLabel: 'Release reserved budget',
      danger: true,
    });
    if (!ok) return;
    await runAction(`release:${e.worker_id}`, async () => {
      await rentalResolve(e.worker_id, true);
      setNoneRefused((prev) => {
        const next = { ...prev };
        delete next[e.worker_id];
        return next;
      });
      setNotice(`Released ${e.label ?? e.worker_id}: its reserved budget is returned.`);
    });
  };

  const checkLimits = async () => {
    const ok = await confirm({
      title: 'Check limits now?',
      paragraphs: [
        'Every rented worker past its duration, budget or idle limit is destroyed now and its charge settled, the same as the automatic check.',
        'Workers whose creation outcome is unknown are only reported. This check never destroys them.',
        'Without your Vast.ai API key, a worker that is due is reported as blocked, not destroyed.',
      ],
      confirmLabel: 'Check and destroy due workers',
      danger: true,
    });
    if (!ok) return;
    await runAction('enforce', async () => {
      const r = await rentalsEnforce();
      setReport(r);
      setNotice('Limit check finished. The result is below.');
    });
  };

  const entries = listing?.rentals ?? [];
  const enforcement = listing?.enforcement ?? RENTAL_ENFORCEMENT_TEXT;

  return (
    <div className="set-section rp-panel">
      <h3>Rented workers and their limits</h3>
      <Notice tone="warn" className="rp-notice">
        <div>
          <p style={{ margin: 0 }}><strong>{RENTAL_LEAD}</strong> {RENTAL_SUMMARY}</p>
          <p style={{ margin: '6px 0 0' }}>What to do: destroy any worker you no longer need with Destroy now, and keep {hostAppName()} open while workers run.</p>
          <details style={{ marginTop: 6 }}>
            <summary>Details</summary>
            <p className="mp-model-meta" style={{ margin: '4px 0 0' }}>Engine statement, for support: {enforcement}</p>
          </details>
        </div>
      </Notice>
      {error ? <Notice tone="bad">{error}</Notice> : null}
      {notice ? <Notice tone="info">{notice}</Notice> : null}
      <Card flush className="group-box">
        {loading && !listing ? (
          <div className="setting"><span style={{ color: 'var(--ink-3)' }}>Loading rented workers…</span></div>
        ) : null}
        {!loading && listing && entries.length === 0 ? (
          <div className="setting"><span style={{ color: 'var(--ink-3)' }}>No rented workers. Nothing is charging from this app.</span></div>
        ) : null}
        {entries.map((e) => {
          const record = records[e.worker_id];
          const figures = rentalFigures(e, record);
          const warnings = rentalWarnings(e);
          const actions = rentalActions(e.rental_status, e.worker_state);
          const settled = settlementText(record);
          return (
            <div key={e.worker_id} className="setting mp-worker rp-row" data-rental-status={e.rental_status}>
              <span className="mp-model-main">
                <span className="mp-model-name">{e.label ?? e.worker_id}</span>
                <span className="mp-model-meta">
                  {[
                    `worker ${workerStateText(e.worker_state)}`,
                    e.instance_id != null ? `provider ID ${String(e.instance_id)}` : null,
                    `elapsed ${formatDuration(figures.elapsedSec)}`,
                    `${figures.live ? 'accrued' : 'settled'} ${usd(figures.accruedUsd)}`,
                    figures.rateUsdHr != null ? `${usd(figures.rateUsdHr)}/hr` : null,
                  ].filter(Boolean).join(' · ')}
                </span>
                {limitRows(e, figures).map((row) => (
                  <span className="mp-model-meta" key={row.name}>
                    {row.name}: {row.limit} ({row.now}){row.due ? ` · ${row.due}` : ''}
                  </span>
                ))}
                {settled ? <span className="mp-model-meta">{settled.charAt(0).toUpperCase()}{settled.slice(1)}</span> : null}
                {e.auto_destroyed ? (
                  <span className="mp-model-meta">
                    Destroyed automatically: {reasonText(e.auto_destroyed.reason)}{e.auto_destroyed.at ? ` at ${e.auto_destroyed.at}` : ''}
                  </span>
                ) : null}
                {warnings.map((w) => (
                  <Notice tone="bad" key={w}>{w}</Notice>
                ))}
                {record?.forced_teardown ? <span className="mp-model-meta">{plainEngineText(String(record.forced_teardown.note ?? ''))}</span> : null}
              </span>
              <span className="mp-model-side">
                <Badge tone={badgeTone(rentalStatusTone(e.rental_status))}>{rentalStatusLabel(e.rental_status)}</Badge>
                <span className="mp-actions">
                  {actions.map((a) => {
                    if (a.id === 'destroy') {
                      const key = `destroy:${e.worker_id}`;
                      return (
                        <Button key={a.id} type="button" size="sm" variant="danger"
                          disabled={a.blocker != null || busy === key}
                          title={a.blocker ?? undefined}
                          onClick={() => void destroy(e)}>
                          Destroy now…
                        </Button>
                      );
                    }
                    return (
                      <Button key={a.id} type="button" size="sm"
                        disabled={busy === `resolve:${e.worker_id}`}
                        onClick={() => void resolve(e)}>
                        Find at provider…
                      </Button>
                    );
                  })}
                  {noneRefused[e.worker_id] ? (
                    <Button type="button" size="sm" variant="danger"
                      disabled={busy === `release:${e.worker_id}`}
                      onClick={() => void releaseNone(e)}>
                      Confirm none exists…
                    </Button>
                  ) : null}
                </span>
              </span>
            </div>
          );
        })}
        <div className="setting">
          <span style={{ color: 'var(--ink-3)' }}>
            The app runs the same check every minute: workers past a limit are destroyed and their charge settled.
          </span>
          <Button type="button" size="sm" disabled={busy === 'enforce'} onClick={() => void checkLimits()}>
            Check limits now…
          </Button>
        </div>
        {report ? (
          <div className="setting" aria-live="polite">
            <span className="mp-model-main">
              {enforceLines(report).map((line, i) => (
                <span className="mp-model-meta" key={i}>{line}</span>
              ))}
            </span>
          </div>
        ) : null}
      </Card>
    </div>
  );
}
