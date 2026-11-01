import { useCallback, useEffect, useState } from 'react';
import { Card, Button, Notice } from '@rightkit/react-shell/react';
import { errorMessage, studio } from '../lib/api';

export interface ProviderBalance {
  provider: string;
  label?: string;
  balance: number | null;
  unit: string | null;
  fetched_at: string;
  error?: string;
}

function formatBalance(row: ProviderBalance): string {
  if (row.balance == null) return 'unavailable';
  if (row.unit === 'USD') return `$${row.balance.toFixed(2)}`;
  const n = Number.isInteger(row.balance) ? String(row.balance) : row.balance.toFixed(1);
  return `${n} ${row.unit ?? 'credits'}`;
}

/** Live provider balances (NB2, fal, WaveSpeed, EvoLink) from `studio.balances.list`. */
export function Balances({ className }: { className?: string }) {
  const [rows, setRows] = useState<ProviderBalance[] | null>(null);
  const [failure, setFailure] = useState('');
  const [busy, setBusy] = useState(false);

  const refresh = useCallback(async () => {
    setBusy(true);
    try {
      setRows(await studio<ProviderBalance[]>('balances.list'));
      setFailure('');
    } catch (error) {
      setFailure(errorMessage(error));
    } finally {
      setBusy(false);
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  return (
    <section className={`balances${className ? ` ${className}` : ''}`} aria-label="Provider balances">
      <header className="balances-head">
        <h3 className="balances-title">Provider balances</h3>
        <Button type="button" size="sm" disabled={busy} onClick={() => void refresh()}>
          {busy ? 'Refreshing' : 'Refresh'}
        </Button>
      </header>
      {failure ? <Notice tone="bad">{failure}</Notice> : null}
      {rows == null && !failure ? <div className="muted">Loading balances</div> : null}
      {rows ? (
        <Card flush className="kv-card">
        <dl className="kv balances-list">
          {rows.map((row) => (
            <div key={row.provider} className="balances-row" title={row.error ?? `Fetched ${row.fetched_at}`}>
              <dt>{row.label ?? row.provider}</dt>
              <dd className={row.error ? 'muted' : undefined}>
                {row.error ? <span title={row.error}>{formatBalance(row)}</span> : formatBalance(row)}
                {row.error ? <span className="muted"> {row.error}</span> : null}
              </dd>
            </div>
          ))}
        </dl>
        </Card>
      ) : null}
    </section>
  );
}
