import { useMemo, useState } from 'react';
import { errorMessage, installLocalImageProfile } from '../../lib/api';
import type { LocalImageInstallProgress, LocalImageProfile } from '../../lib/types';
import { Card, Badge, Button, Notice, ProgressBar } from '@rightkit/react-shell/react';
import { Kv, formatBytes } from '../../components/ui';
import type { LocalImageLive } from './useLocalImageLive';
import { installView, jobStatusLabel, queueEntries, runtimeLabel } from './logic';
import './local.css';

/** Right-hand panel for a local profile: install, runtime, queue, details. */
export function LocalPanel({ profile, live, onInstalled }: { profile: LocalImageProfile; live: LocalImageLive; onInstalled: () => Promise<void> | void }) {
  const ready = profile.install_state === 'installed';
  const { runtime, conversations, installProgress } = live;
  const queue = useMemo(() => queueEntries(conversations), [conversations]);
  return (
    <div className="li-panel">
      {ready ? <Badge tone="ok">Runs on this machine</Badge> : <InstallPanel profile={profile} progress={installProgress[profile.id] ?? null} onInstalled={onInstalled} />}
      <Kv rows={[
        ['Cost', 'Free'],
        ['Runtime', runtime ? `${runtimeLabel(runtime)}${runtime.profile_id ? ` · ${runtime.profile_id}` : ''}` : 'Loading'],
        ['Default size', `${profile.defaults.width} × ${profile.defaults.height}`],
        ['Steps · CFG', `${profile.defaults.sample_steps} · ${profile.defaults.txt_cfg}`],
        ['Sampler', `${profile.defaults.sample_method}${profile.defaults.scheduler ? ` · ${profile.defaults.scheduler}` : ''}`],
        ['Size on disk', formatBytes(profile.total_bytes)],
        ['License', profile.license.name]
      ]} />
      {profile.description ? <Notice tone="info">{profile.description}</Notice> : null}
      <div>
        <h6>Queue{queue.length ? ` · ${queue.length}` : ''}</h6>
        {queue.length === 0 ? <small style={{ color: 'var(--ink-3)' }}>No local jobs running.</small> : null}
        <div className="li-queue">
          {queue.map((q) => (
            <Card flush className="li-q" key={q.job.id}>
              <Button type="button" variant="ghost" size="sm" className="li-q-open" title="Open this conversation" onClick={() => live.setSelectedId(q.conversationId)}>{q.label}</Button>
              <div>
                <span>{jobStatusLabel(q.job)}</span>
                {q.job.state === 'paused' ? <Button type="button" variant="primary" size="sm" onClick={() => void live.resume(q.job)}>Resume</Button> : null}
                <Button type="button" variant="secondary" size="sm" disabled={q.job.state === 'cancelling'} onClick={() => void live.cancel(q.job)}>{q.job.state === 'cancelling' ? 'Cancelling…' : 'Cancel'}</Button>
              </div>
            </Card>
          ))}
        </div>
      </div>
    </div>
  );
}

function InstallPanel({ profile, progress, onInstalled }: { profile: LocalImageProfile; progress: LocalImageInstallProgress | null; onInstalled: () => Promise<void> | void }) {
  const [starting, setStarting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const view = installView(profile, progress);
  const active = starting || profile.install_state === 'installing';
  const pct = view.fraction === null ? null : Math.round(view.fraction * 100);
  const install = async () => {
    setError(null);
    setStarting(true);
    try {
      await installLocalImageProfile(profile.id);
      await onInstalled();
    } catch (e) {
      setError(errorMessage(e).replace(/^[a-z][a-z0-9_]*:\s*/, ''));
    } finally {
      setStarting(false);
    }
  };
  return (
    <Card className={`li-install${view.failed && !active ? ' failed' : ''}`}>
      <div><b>{profile.label}</b> is not installed</div>
      <small>{formatBytes(profile.total_bytes)} download · {profile.components.filter((c) => c.required).length} files · free after install</small>
      {active || (pct ?? 0) > 0 ? (
        <>
          <ProgressBar value={pct ?? 5} max={100} label="Install progress" />
          <small>{pct !== null ? `${pct}% · ` : ''}{view.text || 'starting'}</small>
        </>
      ) : null}
      {error || (view.failed && !active && view.error) ? <Notice tone="bad">{error ?? view.error}</Notice> : null}
      <Button type="button" variant="primary" disabled={active} onClick={() => void install()}>{active ? 'Installing…' : view.failed ? 'Retry install' : 'Install model'}</Button>
    </Card>
  );
}
