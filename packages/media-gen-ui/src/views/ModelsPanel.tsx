import { useCallback, useEffect, useMemo, useState } from 'react';
import { errorMessage, studio } from '../lib/api.js';
import {
  actionBlocker,
  budgetSummary,
  driverLabel,
  gbLabel,
  fitLabel,
  fitTone,
  groupByModality,
  hardwareSummary,
  installActions,
  installProgressPct,
  installProgressText,
  installStateLabel,
  needsProvision,
  readinessLabel,
  readinessTone,
  revisionLabel,
  workerActions,
  workerStateLabel,
  workerStateTone,
  type GovernorStatus,
  type HardwareProfile,
  type InstallProgress,
  type InstallRecord,
  type ModelEntry,
  type RentedWorker,
  type WorkerAction,
} from '../lib/model-management.js';
import { Card, Badge, Button, Checkbox, Input, Notice, ProgressBar, SegmentedControl, Select } from '@rightkit/react-shell/react';
import { RentalsPanel } from './RentalsPanel.js';

interface CatalogResponse {
  hardware: HardwareProfile;
  entries: ModelEntry[];
  routes: Record<string, { route: string; model?: string | null; revision?: string | null; set_at?: string }>;
  capabilities: string[];
  installs?: Record<string, InstallRecord>;
  progress?: Record<string, InstallProgress>;
}

interface VastOffer {
  offer_id?: number;
  gpu?: string;
  num_gpus?: number;
  vram_gb?: number;
  dph_total?: number;
  disk_gb?: number;
  reliability?: number;
}

interface WorkersResponse {
  workers: RentedWorker[];
  driver: string;
}

const ROUTE_OPTIONS = [
  { value: 'auto', label: 'Auto' },
  { value: 'local', label: 'Local' },
  { value: 'api', label: 'API' },
  { value: 'rented', label: 'Rented' },
];

type ChipTone = 'ok' | 'bad' | 'warn' | 'run' | 'local';
const badgeTone = (t?: ChipTone): 'neutral' | 'accent' | 'ok' | 'warn' | 'bad' => (t === 'run' ? 'accent' : t === 'local' ? 'ok' : t ?? 'neutral');


export function ModelsPanel() {
  const [catalog, setCatalog] = useState<CatalogResponse | null>(null);
  const [gov, setGov] = useState<GovernorStatus | null>(null);
  const [workers, setWorkers] = useState<RentedWorker[]>([]);
  const [driver, setDriver] = useState<string>('none');
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [gpu, setGpu] = useState('');
  const [vram, setVram] = useState('');
  const [budget, setBudget] = useState('');
  const [regDriver, setRegDriver] = useState<'none' | 'ssh' | 'vast' | 'local-fs'>('none');
  const [sshHost, setSshHost] = useState('');
  const [sshUser, setSshUser] = useState('');
  const [sshPort, setSshPort] = useState('');
  const [sshKey, setSshKey] = useState('');
  const [sshWorkdir, setSshWorkdir] = useState('');
  const [remoteCmd, setRemoteCmd] = useState('');
  const [vastImage, setVastImage] = useState('pytorch/pytorch:latest');
  const [vastDisk, setVastDisk] = useState('40');
  const [showRegister, setShowRegister] = useState(false);
  const [provisionFor, setProvisionFor] = useState<string | null>(null);
  const [offerId, setOfferId] = useState('');
  const [maxHr, setMaxHr] = useState('');
  const [provBudget, setProvBudget] = useState('');
  const [maxMin, setMaxMin] = useState('');
  const [authorized, setAuthorized] = useState(false);
  const [showOffers, setShowOffers] = useState(false);
  const [vastKey, setVastKey] = useState('');
  const [offers, setOffers] = useState<VastOffer[]>([]);

  const refresh = useCallback(async () => {
    setError(null);
    try {
      const [cat, ws, g] = await Promise.all([
        studio<CatalogResponse>('models.catalog'),
        studio<WorkersResponse>('models.workers'),
        studio<GovernorStatus>('models.governor.status').catch(() => null),
      ]);
      setCatalog(cat);
      setWorkers(ws.workers ?? []);
      setDriver(ws.driver ?? 'none');
      setGov(g);
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  // Poll while any install is running so progress/cancel state stays live.
  const installs = catalog?.installs ?? {};
  const progress = catalog?.progress ?? {};
  const anyLiveInstall = useMemo(
    () => Object.values(installs).some((r) => r.state === 'installing' || r.state === 'cancelling'),
    [installs],
  );
  useEffect(() => {
    if (!anyLiveInstall) return;
    const t = setInterval(() => void refresh(), 2000);
    return () => clearInterval(t);
  }, [anyLiveInstall, refresh]);

  const groups = useMemo(() => groupByModality(catalog?.entries ?? []), [catalog]);
  const routes = catalog?.routes ?? {};
  const capabilities = catalog?.capabilities ?? [];

  async function run(key: string, fn: () => Promise<unknown>, ok?: string) {
    setBusy(key);
    setNotice(null);
    setError(null);
    try {
      const result = (await fn()) as { message?: string; state?: string } | undefined;
      setNotice(ok ?? result?.message ?? null);
      await refresh();
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(null);
    }
  }

  const setRoute = (capability: string, route: string) =>
    run(`route:${capability}`, () => studio('models.route.set', { capability, route }));

  const workerAction = (w: RentedWorker, action: WorkerAction) =>
    run(`worker:${w.worker_id}:${action}`, () =>
      studio('models.worker.action', { worker_id: w.worker_id, action }),
    );

  const provisionWorker = (w: RentedWorker) =>
    run(`worker:${w.worker_id}:provision`, async () => {
      const oid = Number(offerId);
      const hr = Number(maxHr);
      const bud = Number(provBudget);
      const mins = Number(maxMin);
      if (!authorized) throw new Error('Authorize a billed instance first');
      if (!Number.isFinite(oid) || oid <= 0) throw new Error('Enter a Vast offer id (search offers below)');
      if (!(hr > 0)) throw new Error('Set a max $/hr cap');
      if (!(bud > 0)) throw new Error('Set a total budget');
      if (!(mins > 0)) throw new Error('Set a max duration (minutes)');
      await studio('models.worker.action', {
        worker_id: w.worker_id,
        action: 'provision',
        authorized: true,
        offer_id: oid,
        max_usd_hr: hr,
        budget_usd: bud,
        max_duration_min: mins,
        api_key: vastKey.trim() || undefined,
      });
      setProvisionFor(null);
      setAuthorized(false);
    });

  const searchOffers = () =>
    run('rental:search', async () => {
      const r = (await studio('models.rental.search', {
        api_key: vastKey.trim() || undefined,
      })) as { offers?: VastOffer[] };
      setOffers(r.offers ?? []);
      return { message: `${r.offers?.length ?? 0} offers returned` };
    });

  const registerWorker = () =>
    run('worker:register', async () => {
      const spec: Record<string, unknown> = {};
      if (gpu.trim()) spec.gpu = gpu.trim();
      const v = Number(vram);
      if (Number.isFinite(v) && v > 0) spec.vram_gb = v;
      if (regDriver !== 'none') spec.driver = regDriver;
      if (remoteCmd.trim()) spec.remote_command = remoteCmd.trim();
      if (regDriver === 'ssh') {
        if (!sshHost.trim()) throw new Error('SSH host is required');
        spec.ssh = {
          host: sshHost.trim(),
          user: sshUser.trim() || 'root',
          ...(Number(sshPort) > 0 ? { port: Number(sshPort) } : {}),
          ...(sshKey.trim() ? { key: sshKey.trim() } : {}),
          ...(sshWorkdir.trim() ? { workdir: sshWorkdir.trim() } : {}),
        };
      }
      if (regDriver === 'vast') {
        spec.vast = {
          image: vastImage.trim() || 'pytorch/pytorch:latest',
          disk_gb: Number(vastDisk) > 0 ? Number(vastDisk) : 40,
        };
      }
      const b = Number(budget);
      await studio('models.worker.action', {
        action: 'register',
        spec,
        budget_usd: Number.isFinite(b) && b > 0 ? b : 0,
      });
      setShowRegister(false);
      setGpu('');
      setVram('');
      setBudget('');
      setRemoteCmd('');
      setSshHost('');
      setSshUser('');
      setSshPort('');
      setSshKey('');
      setSshWorkdir('');
    });

  return (
    <div className="models-panel">
      <div className="set-section">
        <h3>This machine</h3>
        <Card flush className="group-box">
          <div className="setting">
            <span>Hardware profile</span>
            <span style={{ color: 'var(--ink-2)' }}>
              {catalog ? (
                <>
                  {hardwareSummary(catalog.hardware)}
                  {catalog.hardware.profile ? ` · ${catalog.hardware.profile}` : ''}
                  {catalog.hardware.source === 'override' ? ' (injected)' : ''}
                </>
              ) : loading ? 'Detecting…' : 'Unknown'}
            </span>
          </div>
          <div className="setting">
            <span style={{ color: 'var(--ink-3)', fontSize: 12 }}>
              Fit is estimated against this profile; models marked “Measured here” have a recorded run on matching hardware. Larger hardware is supported by re-probing — these are not permanent limits.
            </span>
            <Button type="button" size="sm" disabled={busy === 'refresh'} onClick={() => run('refresh', refresh)}>Re-probe</Button>
          </div>
        </Card>
      </div>

      {error ? <Notice tone="bad">{error}</Notice> : null}
      {notice ? <Notice tone="info">{notice}</Notice> : null}

      <div className="set-section">
        <h3>Memory</h3>
        <p>Resident models and free RAM. Heavy local jobs are admitted by the memory governor — it unloads idle models or queues work instead of exhausting memory.</p>
        <Card flush className="group-box">
          <div className="setting">
            <span>RAM</span>
            <span style={{ color: 'var(--ink-2)' }}>
              {gov?.ram ? `${gbLabel(gov.ram.free_gb)} free · ${gbLabel(gov.ram.resident_gb)} in loaded models` : loading ? 'Reading…' : 'Unavailable'}
            </span>
          </div>
          {(gov?.loaded ?? []).length === 0 ? (
            <div className="setting"><span style={{ color: 'var(--ink-3)' }}>No models resident in memory.</span></div>
          ) : (
            (gov?.loaded ?? []).map((m) => (
              <div className="setting mp-model" key={`${m.runtime}:${m.model_id}`}>
                <span className="mp-model-main">
                  <span className="mp-model-name">{m.model_id}</span>
                  <span className="mp-model-meta">
                    {[m.runtime, gbLabel(m.rss_gb), m.idle ? 'idle' : 'busy'].filter(Boolean).join(' · ')}
                  </span>
                </span>
                <span className="mp-model-side">
                  <span className="mp-actions">
                    <Button type="button" size="sm"
                      disabled={busy === `gov:unload:${m.runtime}:${m.model_id}`}
                      title={m.idle ? undefined : 'Model is busy — unloading may interrupt a running job'}
                      onClick={() => run(`gov:unload:${m.runtime}:${m.model_id}`, () =>
                        studio('models.governor.unload', { runtime: m.runtime, model_id: m.model_id }),
                        `Unloaded ${m.model_id}`)}>
                      Unload
                    </Button>
                  </span>
                </span>
              </div>
            ))
          )}
          {(gov?.grants ?? []).length > 0 ? (
            <div className="setting">
              <span style={{ color: 'var(--ink-3)', fontSize: 12 }}>
                Active heavy-job grants: {(gov?.grants ?? []).map((g) => `${g.job ?? g.id} (${gbLabel(g.need_gb)})`).join(', ')}
              </span>
            </div>
          ) : null}
        </Card>
      </div>

      <div className="set-section">
        <h3>Models</h3>
        <p>Revisions, quantizations and fit per model. Python-worker engines are labelled External worker — they are never bundled native engines.</p>
        {loading && !catalog ? (
          <Card flush className="group-box"><div className="setting"><span style={{ color: 'var(--ink-3)' }}>Loading model catalog…</span></div></Card>
        ) : null}
        {!loading && !catalog && !error ? (
          <Card flush className="group-box"><div className="setting"><span style={{ color: 'var(--ink-3)' }}>No model catalog returned.</span></div></Card>
        ) : null}
        {groups.map((g) => (
          <Card flush className="group-box" key={g.modality}>
            <div className="setting mp-group-head"><span className="mp-modality">{g.modality}</span></div>
            {g.entries.map((e) => {
              const unloadBlocker = actionBlocker('unload', e);
              const rec = installs[e.id];
              const prog = progress[e.id];
              const live = rec?.state === 'installing' || rec?.state === 'pausing' || rec?.state === 'cancelling';
              const pct = installProgressPct(prog);
              const acts = installActions(e, rec);
              const doInstallAction = (a: string) => {
                const key = `install:${e.id}:${a}`;
                if (a === 'install') return run(key, () => studio('models.install', { id: e.id }));
                if (a === 'upgrade') return run(key, () => studio('models.install', { id: e.id, force: true }));
                if (a === 'pause') return run(key, () => studio('models.install.pause', { id: e.id }));
                if (a === 'cancel') return run(key, () => studio('models.install.cancel', { id: e.id }));
                if (a === 'uninstall')
                  return run(key, () => studio('models.install.uninstall', { id: e.id }),
                    'Files removed — install is fully reversible');
                if (a === 'rollback')
                  return run(key, () => studio('models.install.rollback', { id: e.id }));
                return Promise.resolve();
              };
              return (
                <div className="setting mp-model" key={e.id}>
                  <span className="mp-model-main">
                    <span className="mp-model-name">{e.label ?? e.id}</span>
                    <span className="mp-model-meta">
                      {[e.engine, revisionLabel(e), e.license?.commercial === false ? 'non-commercial' : null]
                        .filter(Boolean)
                        .join(' · ')}
                    </span>
                    {rec ? (
                      <span className="mp-model-meta">{installStateLabel(rec)}</span>
                    ) : null}
                    {live ? (
                      <span className="mp-progress">
                        <ProgressBar value={pct ?? 5} max={100} label={`${e.id} install progress`} />
                        <span className="mp-progress-text">{installProgressText(prog)}</span>
                      </span>
                    ) : null}
                  </span>
                  <span className="mp-model-side">
                    <Badge tone={badgeTone(readinessTone(e))}>{readinessLabel(e)}</Badge>
                    <Badge tone={badgeTone(fitTone(e))}>{fitLabel(e)}</Badge>
                    <span className="mp-actions">
                      <Button type="button" size="sm" disabled={busy === `probe:${e.id}`}
                        onClick={() => run(`probe:${e.id}`, () => studio('models.probe', { id: e.id }))}>
                        Probe
                      </Button>
                      {acts.map((a) => (
                        <Button key={a} type="button" size="sm"
                          disabled={busy === `install:${e.id}:${a}`}
                          onClick={() => void doInstallAction(a)}>
                          {a === 'install' ? (rec?.state === 'paused' || rec?.state === 'cancelled' ? 'Resume' : 'Install')
                            : a === 'upgrade' ? 'Upgrade' : a === 'uninstall' ? 'Uninstall'
                            : a === 'rollback' ? 'Rollback' : a === 'pause' ? 'Pause' : 'Cancel'}
                        </Button>
                      ))}
                      <Button type="button" size="sm" disabled={unloadBlocker != null || busy === `unload:${e.id}`}
                        title={unloadBlocker ?? undefined}
                        onClick={() => run(`unload:${e.id}`, () => studio('models.unload', { id: e.id }))}>
                        Unload
                      </Button>
                    </span>
                  </span>
                </div>
              );
            })}
          </Card>
        ))}
      </div>

      <div className="set-section">
        <h3>Routing</h3>
        <p>Preferred execution route per capability. “Auto” follows the current fit and spend cap; choices persist in the engine store.</p>
        <Card flush className="group-box">
          {capabilities.length === 0 && !loading ? (
            <div className="setting"><span style={{ color: 'var(--ink-3)' }}>No capabilities reported.</span></div>
          ) : null}
          {capabilities.map((cap) => (
            <div className="setting" key={cap}>
              <span style={{ textTransform: 'capitalize' }}>{cap}</span>
              <SegmentedControl
                label={`${cap} route`}
                value={routes[cap]?.route ?? 'auto'}
                options={ROUTE_OPTIONS}
                onChange={(r) => void setRoute(cap, r)}
              />
            </div>
          ))}
        </Card>
      </div>

      <RentalsPanel />

      <div className="set-section">
        <h3>Rented workers</h3>
        <p>
          Stage, reconnect, retrieve and tear down remote GPU workers. SSH drivers drive a host you authorize;
          Vast instances are only created through the explicit authorized Provision action below — nothing here
          bills silently. Default remote driver: <code>{driver}</code>
          {driver === 'none' ? ' (no remote configured — actions report the honest error)' : ''}.
        </p>
        <Card flush className="group-box">
          {workers.length === 0 ? (
            <div className="setting"><span style={{ color: 'var(--ink-3)' }}>No rented workers registered.</span></div>
          ) : null}
          {workers.map((w) => (
            <div key={w.worker_id}>
              <div className="setting mp-worker">
                <span className="mp-model-main">
                  <span className="mp-model-name">{w.label ?? w.worker_id}</span>
                  <span className="mp-model-meta">
                    {[driverLabel(w), w.spec?.gpu, w.spec?.vram_gb ? `${w.spec.vram_gb} GB` : null, budgetSummary(w)]
                      .filter(Boolean).join(' · ')}
                  </span>
                  {(w.outputs ?? []).length > 0 ? (
                    <span className="mp-model-meta">
                      {w.outputs!.filter((o) => o.hash_verified).length}/{w.outputs!.length} outputs hash-verified
                    </span>
                  ) : null}
                </span>
                <span className="mp-model-side">
                  <Badge tone={badgeTone(workerStateTone(w))}>{workerStateLabel(w)}</Badge>
                  <span className="mp-actions">
                    {workerActions(w).map((a) => (
                      <Button key={a} type="button" size="sm"
                        disabled={busy === `worker:${w.worker_id}:${a}`}
                        title={a === 'provision' ? 'Creates a billed Vast instance — requires explicit authorization' : undefined}
                        onClick={() => (a === 'provision' ? setProvisionFor(provisionFor === w.worker_id ? null : w.worker_id) : void workerAction(w, a))}>
                        {a === 'provision' ? 'Provision…' : a}
                      </Button>
                    ))}
                  </span>
                </span>
              </div>
              {needsProvision(w) && provisionFor === w.worker_id ? (
                <div className="setting mp-provision">
                  <span className="mp-model-main">
                    <Input aria-label="Offer id" placeholder="Offer id" inputMode="numeric" value={offerId} onChange={(e) => setOfferId(e.target.value)} />
                    <Input aria-label="Max dollars per hour" placeholder="Max $/hr" inputMode="decimal" value={maxHr} onChange={(e) => setMaxHr(e.target.value)} />
                    <Input aria-label="Total budget USD" placeholder="Budget $" inputMode="decimal" value={provBudget} onChange={(e) => setProvBudget(e.target.value)} />
                    <Input aria-label="Max duration minutes" placeholder="Max minutes" inputMode="numeric" value={maxMin} onChange={(e) => setMaxMin(e.target.value)} />
                    <Checkbox checked={authorized} onChange={setAuthorized} label="I authorize creating one billed instance at or under these caps" />
                  </span>
                  <span className="mp-actions">
                    <Button type="button" size="sm" disabled={!authorized || busy === `worker:${w.worker_id}:provision`}
                      onClick={() => void provisionWorker(w)}>
                      Create instance
                    </Button>
                    <Button type="button" size="sm" onClick={() => setProvisionFor(null)}>Cancel</Button>
                  </span>
                </div>
              ) : null}
            </div>
          ))}
          <div className="setting">
            <span style={{ color: 'var(--ink-3)' }}>Search Vast offers (API key used for this call only — never stored).</span>
            <span className="mp-actions">
              <Button type="button" size="sm" onClick={() => setShowOffers(!showOffers)}>
                {showOffers ? 'Hide offers' : 'Search offers…'}
              </Button>
            </span>
          </div>
          {showOffers ? (
            <div className="setting mp-register">
              <span className="mp-model-main">
                <Input aria-label="Vast API key" type="password" placeholder="Vast API key" value={vastKey} onChange={(e) => setVastKey(e.target.value)} />
                {offers.length > 0 ? (
                  <span className="mp-offers">
                    {offers.slice(0, 10).map((o) => (
                      <Button key={o.offer_id} type="button" variant="ghost" size="sm" className="mp-offer"
                        onClick={() => setOfferId(String(o.offer_id ?? ''))}
                        title="Copy offer id into the provision form">
                        #{o.offer_id} {o.gpu} {o.vram_gb ? `${Math.round(o.vram_gb)} GB` : ''} ${o.dph_total?.toFixed(3)}/hr
                      </Button>
                    ))}
                  </span>
                ) : null}
              </span>
              <span className="mp-actions">
                <Button type="button" size="sm" disabled={busy === 'rental:search'} onClick={() => void searchOffers()}>
                  Search
                </Button>
              </span>
            </div>
          ) : null}
          {showRegister ? (
            <div className="setting mp-register">
              <span className="mp-model-main">
                <Select label="Driver" value={regDriver} onChange={setRegDriver} options={[
                  { value: 'none', label: 'No remote driver (record only)' },
                  { value: 'ssh', label: 'SSH host' },
                  { value: 'vast', label: 'Vast.ai instance' },
                  { value: 'local-fs', label: 'Local test driver' },
                ]} />
                <Input aria-label="GPU type" placeholder="GPU (e.g. RTX 4090)" value={gpu} onChange={(e) => setGpu(e.target.value)} />
                <Input aria-label="VRAM GB" placeholder="VRAM GB" inputMode="decimal" value={vram} onChange={(e) => setVram(e.target.value)} />
                <Input aria-label="Budget USD" placeholder="Budget $" inputMode="decimal" value={budget} onChange={(e) => setBudget(e.target.value)} />
                {regDriver === 'ssh' ? (
                  <>
                    <Input aria-label="SSH host" placeholder="SSH host" value={sshHost} onChange={(e) => setSshHost(e.target.value)} />
                    <Input aria-label="SSH user" placeholder="SSH user (root)" value={sshUser} onChange={(e) => setSshUser(e.target.value)} />
                    <Input aria-label="SSH port" placeholder="Port (22)" inputMode="numeric" value={sshPort} onChange={(e) => setSshPort(e.target.value)} />
                    <Input aria-label="SSH key path" placeholder="Key path (~/.ssh/id_ed25519)" value={sshKey} onChange={(e) => setSshKey(e.target.value)} />
                    <Input aria-label="Remote workdir" placeholder="Remote workdir (~/genright)" value={sshWorkdir} onChange={(e) => setSshWorkdir(e.target.value)} />
                  </>
                ) : null}
                {regDriver === 'vast' ? (
                  <>
                    <Input aria-label="Container image" placeholder="Image" value={vastImage} onChange={(e) => setVastImage(e.target.value)} />
                    <Input aria-label="Disk GB" placeholder="Disk GB" inputMode="numeric" value={vastDisk} onChange={(e) => setVastDisk(e.target.value)} />
                  </>
                ) : null}
                <Input aria-label="Remote command" placeholder="Remote command (run on worker)" value={remoteCmd} onChange={(e) => setRemoteCmd(e.target.value)} />
              </span>
              <span className="mp-actions">
                <Button type="button" size="sm" disabled={busy === 'worker:register'} onClick={() => void registerWorker()}>Register</Button>
                <Button type="button" size="sm" onClick={() => setShowRegister(false)}>Cancel</Button>
              </span>
            </div>
          ) : (
            <div className="setting">
              <span style={{ color: 'var(--ink-3)' }}>Register an SSH host you own, or a Vast worker you will provision explicitly.</span>
              <Button type="button" size="sm" onClick={() => setShowRegister(true)}>Register worker…</Button>
            </div>
          )}
        </Card>
      </div>
    </div>
  );
}
