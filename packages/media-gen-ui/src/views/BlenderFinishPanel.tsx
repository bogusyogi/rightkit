// Blender product spec finish approval. The owner reviews a product spec file, then confirms its
// finishes: studio.blender.spec.preflight returns the spec's draft id and a challenge (approves
// nothing), the app mints the token through approveDraft('blender_finish', ...), and the engine's
// studio.blender.spec.approve writes the confirmation into the spec. Local Blender jobs that carry
// this spec refuse adoption until that confirmation exists.
import { useState } from 'react';
import { Card, Button, Input, Notice, Pill } from '@rightkit/react-shell/react';
import { approveDraft, errorText, studio } from '../lib/api.js';

interface SpecPreflight {
  path: string;
  draft_id: string;
  spec?: Record<string, unknown>;
  approval_challenge?: unknown;
}

interface SpecApproval {
  path: string;
  draft_id: string;
  confirmed: boolean;
  approval?: unknown;
}

function specLabel(spec: Record<string, unknown> | undefined): string {
  if (!spec) return 'Product spec';
  const name = spec.name ?? spec.title ?? spec.product;
  return typeof name === 'string' && name ? name : 'Product spec';
}

function finishRows(spec: Record<string, unknown> | undefined): { id: string; approved: boolean }[] {
  const finishes = spec?.finishes;
  if (!finishes || typeof finishes !== 'object') return [];
  return Object.entries(finishes as Record<string, unknown>).map(([id, value]) => ({
    id,
    approved: Boolean(value && typeof value === 'object' && (value as { approved?: unknown }).approved === true)
  }));
}

export function BlenderFinishPanel() {
  const [path, setPath] = useState('');
  const [preflight, setPreflight] = useState<SpecPreflight | null>(null);
  const [result, setResult] = useState<SpecApproval | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<'preflight' | 'approve' | null>(null);

  // A preflight binds to the path it was run for; editing the path drops the old draft.
  const trimmed = path.trim();
  const current = preflight && preflight.path === trimmed ? preflight : null;

  async function runPreflight() {
    if (!trimmed) return;
    setBusy('preflight');
    setResult(null);
    try {
      const next = await studio<SpecPreflight>('blender.spec.preflight', { path: trimmed });
      setPreflight(next);
      setError(null);
    } catch (e) {
      setPreflight(null);
      setError(errorText(e));
    } finally {
      setBusy(null);
    }
  }

  // Approve only the draft the user was shown: the draft id is also the displayed hash.
  async function approveFinish() {
    if (!current) return;
    const shown = current;
    setBusy('approve');
    try {
      const approved = await approveDraft<SpecApproval>('blender_finish', shown.draft_id, shown.draft_id, { path: shown.path });
      setResult(approved);
      setError(null);
      setPreflight(null);
    } catch (e) {
      setResult(null);
      setError(errorText(e));
    } finally {
      setBusy(null);
    }
  }

  const rows = finishRows(current?.spec);

  return (
    <Card flush className="group-box pr-blender gb-mt12">
      <div className="eyebrow">Blender finish approval</div>
      <p className="lede" style={{ margin: '4px 0 10px' }}>
        Review a Blender product spec and approve its finishes. Local Blender jobs refuse to adopt renders from a spec that is not confirmed.
      </p>

      <div className="toolbar">
        <Input
          aria-label="Product spec file"
          placeholder="Path to the product spec (.json)"
          value={path}
          onChange={(e) => setPath(e.target.value)}
          style={{ flex: 1, minWidth: 0 }}
        />
        <Button type="button" variant="secondary" onClick={runPreflight} disabled={!trimmed || busy !== null}>
          {busy === 'preflight' ? 'Checking…' : 'Check spec'}
        </Button>
      </div>

      {current ? (
        <Notice tone="info">
          <div className="pr-top"><b>{specLabel(current.spec)}</b><Pill>Draft {current.draft_id.slice(0, 12)}</Pill></div>
          {rows.length ? (
            <ul aria-label="Finishes" style={{ margin: '6px 0', paddingLeft: 18 }}>
              {rows.map((row) => (
                <li key={row.id}>{row.id}: {row.approved ? 'approved' : 'not approved'}</li>
              ))}
            </ul>
          ) : (
            <p className="pr-sub">This spec lists no finishes.</p>
          )}
          <Button type="button" variant="secondary" onClick={approveFinish} disabled={busy !== null}>
            {busy === 'approve' ? 'Approving…' : 'Approve finish'}
          </Button>
        </Notice>
      ) : null}

      {result ? (
        <Notice tone="info">
          {result.confirmed ? 'Finish approved. The spec now carries the confirmation.' : 'The engine did not confirm the spec.'}
        </Notice>
      ) : null}

      {error ? <Notice tone="bad">{error}</Notice> : null}
    </Card>
  );
}
