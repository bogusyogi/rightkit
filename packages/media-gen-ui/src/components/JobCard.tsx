import { assetUrl, isHeldJob } from '../lib/api.js';
import { jobIsActive, type SessionJob } from '../lib/app-state.js';
import { heldLabel, isRecoveredCode } from '../lib/held-job.js';
import { formatDuration, stageLabel } from '../lib/gen-params.js';
import { Badge, Button, ProgressBar } from '@rightkit/react-shell/react';
import { formatBytes, usd } from './ui.js';
import { ErrorNote } from './ErrorNote.js';
import { MediaPlayer } from '@rightkit/media-ui';

export function statusText(job: SessionJob): string {
  if (jobIsActive(job)) {
    if (job.progress !== null && job.progress !== undefined && job.progress > 0) return `${Math.round(job.progress * 100)}%`;
    return stageLabel(job.stage ?? (job.status === 'queued' ? 'submitted' : null), job.stageMessage);
  }
  if (isHeldJob(job.status)) return heldLabel(isRecoveredCode(job.errorCode));
  if (job.status === 'succeeded') return job.estUsd ? usd(job.estUsd) : 'free';
  if (job.status === 'cancelled') return 'Cancelled';
  return job.error ? job.error : 'Failed';
}

export function JobMeta({ job, onCancel }: { job: SessionJob; onCancel?: (job: SessionJob) => void }) {
  const failed = job.status === 'failed' || job.status === 'interrupted';
  const active = jobIsActive(job);
  return (
    <div className="job-meta">
      <Badge tone={job.source === 'local' ? 'ok' : undefined}>{job.label}{job.source === 'local' ? ' · local' : ''}</Badge>
      {job.aspect ? <Badge>{job.aspect}</Badge> : null}
      {job.source === 'cloud' ? <Badge>{active ? `est. ${usd(job.estUsd)}` : usd(job.estUsd)}</Badge> : null}
      <span className={failed ? 'err-text' : undefined}>{failed ? 'Failed' : job.status === 'succeeded' && job.source === 'cloud' ? 'Done' : statusText(job)}</span>
      {active && onCancel ? <Button type="button" variant="ghost" size="sm" onClick={() => onCancel(job)}>Cancel</Button> : null}
    </div>
  );
}

function mediaLine(asset: SessionJob['assets'][number]): string {
  const parts: string[] = [];
  if (asset.width && asset.height) parts.push(`${asset.width} × ${asset.height}`);
  if (asset.durationS != null) parts.push(formatDuration(asset.durationS));
  if (asset.bytes) parts.push(formatBytes(asset.bytes));
  return parts.join(' · ');
}

function Output({ job, asset, onReuse }: { job: SessionJob; asset: SessionJob['assets'][number]; onReuse?: (job: SessionJob) => void }) {
  const url = assetUrl(asset.path);
  const media = asset.media || job.kind;
  const info = mediaLine(asset);
  const actions = (
    <div className="acts">
      {info ? <span className="media-info">{info}</span> : null}
      <a href={url} target="_blank" rel="noreferrer">Open</a>
      {onReuse ? <Button type="button" variant="ghost" size="sm" onClick={() => onReuse(job)}>Reuse prompt</Button> : null}
    </div>
  );
  if (media === 'video' || media === 'avatar' || media === 'lipsync') {
    return <div className="out"><MediaPlayer src={url} kind="video" title={job.prompt} info={info || undefined} />{actions}</div>;
  }
  if (media === 'audio' || media === 'voice' || media === 'music' || media === 'sfx') {
    return <div className="out audio"><MediaPlayer src={url} kind="audio" title={job.prompt} info={info || undefined} />{actions}</div>;
  }
  return <div className="out"><img src={url} alt={job.prompt} />{actions}</div>;
}

export function JobOutputs({ job, onReuse, onRetry, onOpenKeys, onFallback }: { job: SessionJob; onReuse?: (job: SessionJob) => void; onRetry?: (job: SessionJob) => void; onOpenKeys?: () => void; onFallback?: (job: SessionJob) => void }) {
  if (jobIsActive(job)) {
    const pct = job.progress && job.progress > 0 ? Math.round(job.progress * 100) : null;
    return (
      <div className="outs" style={{ gridTemplateColumns: '1fr' }}>
        <div className="out running">
          <div style={{ textAlign: 'center' }}>
            {stageLabel(job.stage ?? 'submitted', job.stageMessage)}
            {pct === null ? <div className="bar"><i className="indet" /></div> : <ProgressBar value={pct} max={100} label="Job progress" />}
          </div>
        </div>
      </div>
    );
  }
  if (isHeldJob(job.status)) {
    return (
      <div className="outs" style={{ gridTemplateColumns: '1fr' }}>
        <div className="out running failed-out">
          <div style={{ textAlign: 'center', padding: '0 12px' }}>{heldLabel(isRecoveredCode(job.errorCode))}</div>
        </div>
      </div>
    );
  }
  if (job.status !== 'succeeded') {
    return (
      <div className="outs" style={{ gridTemplateColumns: '1fr' }}>
        <div className="out running failed-out">
          <div style={{ textAlign: 'center', padding: '0 12px' }}>
            {job.status === 'cancelled' ? 'Cancelled' : job.error ? <ErrorNote code={job.errorCode} message={job.errorMessage ?? job.error} onOpenKeys={onOpenKeys} onFallback={onFallback ? () => onFallback(job) : undefined} /> : 'Failed'}
            {onRetry ? <div><Button type="button" variant="ghost" size="sm" style={{ marginTop: 4 }} onClick={() => onRetry(job)}>Retry</Button></div> : null}
          </div>
        </div>
      </div>
    );
  }
  if (job.assets.length === 0) {
    return <div className="outs" style={{ gridTemplateColumns: '1fr' }}><div className="out running">No output files</div></div>;
  }
  return (
    <div className="outs" style={job.kind === 'image' ? undefined : { gridTemplateColumns: '1fr' }}>
      {job.assets.map((a) => <Output key={a.asset_id} job={job} asset={a} onReuse={onReuse} />)}
    </div>
  );
}
