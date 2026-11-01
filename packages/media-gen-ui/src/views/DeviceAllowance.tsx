import { Card, Button, Input, Notice } from '@rightkit/react-shell/react';
import { useCallback, useEffect, useState } from 'react';
import { fetchSpendAllowance, setSpendAllowance, type DeviceAllowanceView } from '../lib/api';
import { EngineError, toError } from '../lib/engine-error';
import { formatUsd } from './spend-reconcile-logic';
import { DEVICE_ALLOWANCE_SCOPE, allowanceStatusText, auditRowText, parseAllowanceAmount, sharedAllowanceText } from './device-allowance-logic';

/** The engine's message without its code: codes are not shown as copy. */
function errorMessage(error: unknown): string {
  const e = toError(error);
  return e instanceof EngineError ? e.detail : e.message;
}

/** Settings > Spend "This device": this computer's offline allowance, set or cleared through the engine. */
export function DeviceAllowance() {
  const [view, setView] = useState<DeviceAllowanceView | null>(null);
  const [amount, setAmount] = useState('');
  const [note, setNote] = useState('');
  const [msg, setMsg] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const load = useCallback(async () => {
    try {
      setView(await fetchSpendAllowance());
      setError(null);
    } catch (e) {
      setError(errorMessage(e));
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  async function apply(value: number | null) {
    setBusy(true);
    try {
      setView(await setSpendAllowance(value, note.trim() || undefined));
      setMsg(value === null ? 'Allowance cleared for this device' : 'Saved for this device');
      setError(null);
    } catch (e) {
      setMsg(null);
      setError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  }

  function setClicked() {
    const parsed = parseAllowanceAmount(amount);
    if (!parsed.ok) {
      setMsg(null);
      setError(parsed.message);
      return;
    }
    void apply(parsed.value);
  }

  if (!view) {
    return (
      <Card flush className="group-box" label="This device allowance">
        <div className="setting"><span>{error ?? 'Loading this device…'}</span></div>
      </Card>
    );
  }

  return (
    <>
      <Card flush className="group-box" label="This device allowance">
        <div className="setting" style={{ display: 'block' }}>
          <b>This device</b>
          <p style={{ margin: '6px 0 0', color: 'var(--ink-2)' }}>{DEVICE_ALLOWANCE_SCOPE} It counts only the spend this computer&apos;s ledger records. {sharedAllowanceText(view.shared_allowance)}</p>
          <p style={{ margin: '6px 0 0', color: 'var(--ink-2)' }}>{allowanceStatusText(view)}</p>
        </div>
        <div className="setting"><span>Device ID</span><span className="selectable" style={{ color: 'var(--ink-2)', overflowWrap: 'anywhere', textAlign: 'right' }}>{view.device_id}</span></div>
        <div className="setting"><span>Allowance</span><span style={{ color: 'var(--ink-2)' }}>{view.device_allowance_usd === null ? 'Not set' : formatUsd(view.device_allowance_usd)}</span></div>
        <div className="setting"><span>Counted since set</span><span style={{ color: 'var(--ink-2)' }}>{view.counted_usd === null ? 'Not set' : formatUsd(view.counted_usd)}</span></div>
        <div className="setting"><span>Remaining</span><span style={{ color: 'var(--ink-2)' }}>{view.remaining_usd === null ? 'Not set' : formatUsd(view.remaining_usd)}</span></div>
        <div className="setting"><span>Set at</span><span style={{ color: 'var(--ink-2)' }}>{view.set_at ?? 'Never set'}</span></div>
        <div className="setting">
          <span>New allowance</span>
          <span className="cap-input">
            <span style={{ color: 'var(--ink-3)' }}>$</span>
            <Input aria-label="Device allowance in dollars" inputMode="decimal" placeholder="amount, for example 5" value={amount} onChange={(e) => { setAmount(e.target.value); setMsg(null); }} onKeyDown={(e) => { if (e.key === 'Enter') setClicked(); }} />
            <Button type="button" size="sm" disabled={busy} onClick={setClicked}>Set allowance</Button>
            {view.device_allowance_usd !== null ? <Button type="button" size="sm" disabled={busy} onClick={() => void apply(null)}>Clear</Button> : null}
          </span>
        </div>
        <div className="setting">
          <span>Note (optional)</span>
          <Input aria-label="Device allowance note" maxLength={500} value={note} onChange={(e) => setNote(e.target.value)} placeholder="for example, laptop share" />
        </div>
        <div className="setting"><span style={{ color: 'var(--ink-3)' }}>Last changes on this device</span></div>
        {view.audit.length === 0 ? <div className="setting"><span style={{ color: 'var(--ink-3)' }}>No changes recorded</span></div> : null}
        {view.audit.map((row) => (
          <div className="setting" key={row.id}><span className="pr-sub" style={{ color: 'var(--ink-2)', overflowWrap: 'anywhere' }}>{auditRowText(row)}</span></div>
        ))}
      </Card>
      {error ? <Notice tone="bad">{error}</Notice> : null}
      {msg ? <div className="foot-note" style={{ textAlign: 'left' }}>{msg}</div> : null}
    </>
  );
}
