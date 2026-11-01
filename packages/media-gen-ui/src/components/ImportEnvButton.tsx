import { useState } from 'react';
import { errorMessage, importCredentialsFromEnv } from '../lib/api.js';
import { Button } from '@rightkit/react-shell/react';
import { providerName } from './ui.js';

/** Development convenience: copy provider keys from environment variables into the keychain. */
export function ImportEnvButton({ onDone }: { onDone: () => void }) {
  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState<string | null>(null);

  async function run() {
    setBusy(true);
    setMsg(null);
    try {
      const r = await importCredentialsFromEnv(false);
      const names = (ids: string[]) => ids.map(providerName).join(', ') || 'none';
      setMsg(`Imported: ${names(r.imported)}. Already saved: ${names(r.skipped_present)}. Not found: ${names(r.missing)}.`);
      onDone();
    } catch (e) {
      setMsg(errorMessage(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="setting">
      <span>
        <Button type="button" size="sm" disabled={busy} onClick={() => void run()}>Import from environment</Button>
        {msg ? <small style={{ display: 'block', color: 'var(--ink-3)', fontSize: 11 }}>{msg}</small> : null}
      </span>
    </div>
  );
}
