import { useState } from 'react';
import { errorMessage, setCredential, testCredential, type CredentialRuntime } from '../lib/api';
import { Badge, Button, Input } from '@rightkit/react-shell/react';
import { PROVIDER_META } from './ui';

type ChipTone = 'ok' | 'bad' | 'warn' | 'run' | 'local';
const badgeTone = (t?: ChipTone): 'neutral' | 'accent' | 'ok' | 'warn' | 'bad' => (t === 'run' ? 'accent' : t === 'local' ? 'ok' : t ?? 'neutral');


/** One provider key row: paste, Test (saves then tests), Remove. Shared by Settings and onboarding. */
export function KeyRow({ id, configured, source, onChanged, compact, needsKey = true, runtime }: { id: string; configured: boolean; source?: 'keychain' | 'environment' | 'local' | null; onChanged: () => void; compact?: boolean; needsKey?: boolean; runtime?: CredentialRuntime | null }) {
  const meta = PROVIDER_META[id] ?? { name: id, desc: '', placeholder: 'API key' };
  const [value, setValue] = useState('');
  const [state, setState] = useState<{ tone: 'ok' | 'bad' | 'warn' | 'run'; text: string } | null>(null);
  const [busy, setBusy] = useState(false);

  async function test() {
    setBusy(true);
    setState({ tone: 'run', text: 'Testing…' });
    try {
      // Local engines take no key; their test checks installation instead.
      if (needsKey && value.trim()) {
        await setCredential(id, value.trim());
        setValue('');
        onChanged();
      } else if (needsKey && !configured) {
        setState({ tone: 'warn', text: 'Paste a key first' });
        return;
      }
      const result = await testCredential(id);
      setState(result.ok ? { tone: 'ok', text: result.message ? `✓ ${result.message}` : '✓ Connected' } : { tone: 'bad', text: result.message || 'Test failed' });
    } catch (e) {
      setState({ tone: 'bad', text: errorMessage(e) });
    } finally {
      setBusy(false);
    }
  }

  async function remove() {
    setBusy(true);
    try {
      await setCredential(id, '');
      setState(null);
      onChanged();
    } catch (e) {
      setState({ tone: 'bad', text: errorMessage(e) });
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className={`key${compact ? '' : ' settings'}`}>
      <div><b style={{ fontWeight: 500 }}>{meta.name}</b><small>{meta.desc}</small></div>
      {needsKey ? (
        <Input
          type="password"
          autoComplete="off"
          aria-label={`${meta.name} API key`}
          placeholder={configured ? (source === 'environment' ? 'set by environment variable' : '•••••••• saved') : meta.placeholder}
          value={value}
          onChange={(e) => setValue(e.target.value)}
          onKeyDown={(e) => { if (e.key === 'Enter') void test(); }}
        />
      ) : null}
      <span className="st" style={{ display: 'flex', gap: 6, justifyContent: 'flex-end', alignItems: 'center' }}>
        {state ? <Badge tone={badgeTone(state.tone)}>{state.text}</Badge> : !needsKey ? (
          runtime == null ? <Badge tone="ok">Local — no key needed</Badge>
          : runtime.ready ? <Badge tone="ok">{runtime.external ? 'Local worker installed' : 'Installed — no key needed'}</Badge>
          : <Badge tone="warn">{runtime.code === 'not_installed' ? 'Not installed' : 'Not ready'}</Badge>
        ) : configured ? <Badge tone="ok">{source === 'environment' ? 'From environment' : 'Saved in keychain'}</Badge> : <Badge>No key</Badge>}
        <Button type="button" size="sm" disabled={busy} onClick={() => void test()}>Test</Button>
        {needsKey && configured && source !== 'environment' ? <Button type="button" size="sm" variant="danger" disabled={busy} onClick={() => void remove()}>Remove</Button> : null}
      </span>
    </div>
  );
}
