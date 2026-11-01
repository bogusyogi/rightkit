import { useEffect, useState } from 'react';
import { errorMessage, setVastKey, vastKeyStatus } from '../lib/api.js';
import { parseVastKey, vastKeyStatusText, type VastKeyStatus } from '../views/vast-key-logic.js';
import { Badge, Button, Input } from '@rightkit/react-shell/react';

/** Settings control that saves or removes the Vast.ai key through the engine's credential path. */
export function VastKeyRow() {
  const [status, setStatus] = useState<VastKeyStatus | null>(null);
  const [value, setValue] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const refresh = () =>
    vastKeyStatus().then(
      (s) => { setStatus(s); },
      (e) => setError(errorMessage(e))
    );
  useEffect(() => { void refresh(); }, []);

  async function save() {
    const parsed = parseVastKey(value);
    if ('error' in parsed) { setError(parsed.error); return; }
    setBusy(true);
    setError(null);
    try {
      setStatus(await setVastKey(parsed.key));
      setValue('');
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  }

  async function remove() {
    setBusy(true);
    setError(null);
    try {
      setStatus(await setVastKey(''));
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="key settings">
      <div><b style={{ fontWeight: 500 }}>Vast.ai (rented GPUs)</b><small>{vastKeyStatusText(status)}</small></div>
      <Input
        type="password"
        autoComplete="off"
        aria-label="Vast.ai API key"
        placeholder={status?.vast_key_stored ? '•••••••• saved' : 'Vast.ai API key'}
        value={value}
        onChange={(e) => setValue(e.target.value)}
        onKeyDown={(e) => { if (e.key === 'Enter') void save(); }}
      />
      <span className="st" style={{ display: 'flex', gap: 6, justifyContent: 'flex-end', alignItems: 'center' }}>
        {error ? <Badge tone="bad">{error}</Badge> : status?.vast_key_stored ? <Badge tone="ok">Saved in keychain</Badge> : <Badge>No key</Badge>}
        <Button type="button" size="sm" disabled={busy} onClick={() => void save()}>Save</Button>
        {status?.vast_key_stored ? <Button type="button" size="sm" variant="danger" disabled={busy} onClick={() => void remove()}>Remove</Button> : null}
      </span>
    </div>
  );
}
