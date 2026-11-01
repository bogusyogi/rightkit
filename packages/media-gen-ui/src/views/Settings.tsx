import { useEffect, useMemo, useState } from 'react';
import { SettingsPanel, mergeSections, sectionIcons, standardSections, type SettingsSection } from '@rightkit/settings-ui';
import '@rightkit/settings-ui/settings.css';
import pkg from '@rightkit-host/package.json';
import { errorMessage, fetchApps, fetchCredentialStatus, setMonthlySpendCap, setSpendCap, type CredentialProvider, type InstalledApp, type SpendPeriodAmounts } from '../lib/api';
import { SpendReconcile } from './SpendReconcile';
import { DeviceAllowance } from './DeviceAllowance';
import { estimatedText, formatUsd, invoiceReconciledText, providerReportedText } from './spend-reconcile-logic';
import { useApp } from '../lib/app-state';
import { doctorHealthy, scalarRows } from '../lib/doctor';
import { platformOS } from '../lib/platform';
import type { SettingsSectionId } from '../lib/nav';
import { settingsBackend } from '../lib/settings-backend';
import { LEGAL_DOCUMENT_SOURCES, THIRD_PARTY_NOTICES, type LegalDocumentId } from '../legal/documents';
import { themeStore } from '../lib/theme';
import { KeyRow } from '../components/KeyRow';
import { VastKeyRow } from '../components/VastKeyRow';
import { Balances } from '../components/Balances';
import { ModelsPanel } from './ModelsPanel';
import { ImportEnvButton } from '../components/ImportEnvButton';
import { LegalDocumentView } from './LegalDocument';
import { Card, Badge, Button, Input, Notice, ProgressBar } from '@rightkit/react-shell/react';
import { usd } from '../components/ui';
import { createAppUpdateClient } from '../lib/app-update';
import { tauriCheckForUpdate } from '../lib/app-update-tauri';
import { relaunch } from '@tauri-apps/plugin-process';
import { licenseClient } from '../lib/license';
import { LicensePlan } from './LicensePlan';
import './workspace.css';
import { hostAppName } from "../appName";

// One client per app run; the Tauri check is the only place that touches the updater plugin.
const updateClient = createAppUpdateClient(tauriCheckForUpdate);

function KeysSection() {
  const app = useApp();
  const [providers, setProviders] = useState<CredentialProvider[]>([]);
  const [keyError, setKeyError] = useState<string | null>(null);
  const load = () =>
    fetchCredentialStatus().then(
      (s) => { setProviders(s.providers); setKeyError(null); void app.refreshCredentials(); },
      (e) => setKeyError(errorMessage(e))
    );
  useEffect(() => { void load(); }, []); // eslint-disable-line react-hooks/exhaustive-deps
  return (
    <div className="set-section">
      <p>Provider keys live in your system keychain, never in project files. Add only what you use. Test saves a pasted key, then checks it with the provider. Keys read from environment variables cannot be removed here; Import from environment copies them into the keychain.</p>
      {keyError ? <Notice tone="bad">{keyError}</Notice> : null}
      <ImportEnvButton onDone={() => void load()} />
      <Card flush className="group-box">
        {providers.length === 0 && !keyError ? <div className="setting"><span>Loading providers…</span></div> : null}
        {providers.map((p) => <KeyRow key={p.id} id={p.id} configured={p.configured} source={p.source} needsKey={p.needs_key !== false} runtime={p.runtime} onChanged={() => void load()} />)}
        <VastKeyRow />
      </Card>
      <div style={{ marginTop: 22 }}><Balances /></div>
    </div>
  );
}

/** One period's amounts, each on its own row: estimated, reserved, settled, cap-counted, provider-reported, invoice-reconciled. */
function AmountRows({ heading, a }: { heading: string; a: SpendPeriodAmounts }) {
  const row = (label: string, value: string) => (
    <div className="setting"><span>{label}</span><span style={{ color: 'var(--ink-2)' }}>{value}</span></div>
  );
  return (
    <>
      <div className="setting"><span style={{ color: 'var(--ink-3)' }}>{heading}</span></div>
      {row('Estimated', estimatedText(a))}
      {row('Reserved', formatUsd(a.reserved_usd))}
      {row('Settled', formatUsd(a.settled_usd))}
      {row('Cap-counted', `${formatUsd(a.cap_counted_usd)} (settled plus open reservations)`)}
      {row('Provider-reported', providerReportedText(a))}
      {row('Invoice-reconciled', invoiceReconciledText(a))}
    </>
  );
}

function SpendSection() {
  const app = useApp();
  const [cap, setCap] = useState('');
  const [monthlyCap, setMonthlyCap] = useState('');
  const [capMsg, setCapMsg] = useState<string | null>(null);
  useEffect(() => {
    if (app.spend) setCap(String(app.spend.cap_usd));
  }, [app.spend?.cap_usd]); // eslint-disable-line react-hooks/exhaustive-deps
  useEffect(() => {
    if (app.spend) setMonthlyCap(app.spend.monthly_cap_usd === null || app.spend.monthly_cap_usd === undefined ? '' : String(app.spend.monthly_cap_usd));
  }, [app.spend?.monthly_cap_usd]); // eslint-disable-line react-hooks/exhaustive-deps

  async function saveCap() {
    const n = Number(cap);
    if (!Number.isFinite(n) || n < 0) {
      setCapMsg('Enter a dollar amount of 0 or more.');
      return;
    }
    try {
      await setSpendCap(n);
      setCapMsg('Saved');
      await app.refreshSpend();
    } catch (e) {
      setCapMsg(errorMessage(e));
    }
  }

  async function saveMonthlyCap() {
    const text = monthlyCap.trim();
    const n = text === '' ? null : Number(text);
    if (n !== null && (!Number.isFinite(n) || n < 0)) {
      setCapMsg('Enter a dollar amount of 0 or more, or leave it empty for no monthly cap.');
      return;
    }
    try {
      await setMonthlySpendCap(n);
      setCapMsg(n === null ? 'Monthly cap cleared' : 'Saved');
      await app.refreshSpend();
    } catch (e) {
      setCapMsg(errorMessage(e));
    }
  }

  const monthSpent = app.spend ? (app.spend.month_counted_usd ?? 0) : 0;
  const monthCap = app.spend?.monthly_cap_usd ?? null;
  const monthPct = monthCap !== null && monthCap > 0 ? Math.min(100, (monthSpent / monthCap) * 100) : 0;
  const dayAmounts = app.spend?.amounts.day ?? null;
  const monthAmounts = app.spend?.amounts.month ?? null;
  const spent = app.spend ? app.spend.actual_usd + app.spend.reserved_usd : 0;
  const pct = app.spend && app.spend.cap_usd > 0 ? Math.min(100, (spent / app.spend.cap_usd) * 100) : 0;
  return (
    <div className="set-section">
      <p>Jobs that would pass the daily cap or the monthly cap (UTC calendar month) are refused before they start.</p>
      {app.spendError ? <Notice tone="bad">{app.spendError}</Notice> : null}
      <Card flush className="group-box">
        <div className="setting"><span>Daily cap</span>
          <span className="cap-input">
            <span style={{ color: 'var(--ink-3)' }}>$</span>
            <Input aria-label="Daily spend cap in dollars" inputMode="decimal" value={cap} onChange={(e) => { setCap(e.target.value); setCapMsg(null); }} onKeyDown={(e) => { if (e.key === 'Enter') void saveCap(); }} />
            <Button type="button" size="sm" onClick={() => void saveCap()}>Save</Button>
          </span>
        </div>
        <div className="setting"><span>Monthly cap</span>
          <span className="cap-input">
            <span style={{ color: 'var(--ink-3)' }}>$</span>
            <Input aria-label="Monthly spend cap in dollars" placeholder="none" inputMode="decimal" value={monthlyCap} onChange={(e) => { setMonthlyCap(e.target.value); setCapMsg(null); }} onKeyDown={(e) => { if (e.key === 'Enter') void saveMonthlyCap(); }} />
            <Button type="button" size="sm" onClick={() => void saveMonthlyCap()}>Save</Button>
          </span>
        </div>
        <div className="setting"><span>Today{app.spend ? ` (${app.spend.day})` : ''}</span><span style={{ color: 'var(--ink-2)' }}>{app.spend ? `${usd(app.spend.actual_usd)} spent · ${usd(app.spend.reserved_usd)} reserved` : 'unknown'}</span></div>
        <div className="setting" style={{ display: 'block' }}><ProgressBar value={pct} max={100} tone={pct > 85 ? 'warn' : 'accent'} label="Daily spend against cap" /></div>
        <div className="setting"><span>This month{app.spend?.month ? ` (${app.spend.month})` : ''}</span><span style={{ color: 'var(--ink-2)' }}>{app.spend ? (monthCap === null ? `${usd(monthSpent)} counted · no monthly cap` : `${usd(monthSpent)} of ${usd(monthCap)} · ${usd(app.spend.monthly_remaining_usd ?? 0)} remaining`) : 'unknown'}</span></div>
        {monthCap !== null ? <div className="setting" style={{ display: 'block' }}><ProgressBar value={monthPct} max={100} tone={monthPct > 85 ? 'warn' : 'accent'} label="Monthly spend against cap" /></div> : null}
        {dayAmounts ? <AmountRows heading={`Amounts today (${app.spend?.day ?? ''})`} a={dayAmounts} /> : null}
        {monthAmounts ? <AmountRows heading={`Amounts this month (${app.spend?.month ?? ''})`} a={monthAmounts} /> : null}
      </Card>
      {capMsg ? <div className="foot-note" style={{ textAlign: 'left' }}>{capMsg}</div> : null}
      <DeviceAllowance />
      <SpendReconcile />
    </div>
  );
}

function EngineSection() {
  const app = useApp();
  const health = doctorHealthy(app.doctor, app.doctorError);
  return (
    <div className="set-section">
      <p>Reported by the engine doctor.</p>
      {app.doctorError ? <Notice tone="bad">{app.doctorError}</Notice> : null}
      <Card flush className="group-box">
        <div className="setting"><span>Status</span><Badge tone={health === 'ok' ? 'ok' : health === 'bad' ? 'bad' : undefined}>{health === 'ok' ? 'Healthy' : health === 'bad' ? 'Problem' : 'Checking…'}</Badge></div>
        {scalarRows(app.doctor).filter(([k]) => k !== 'Ok').map(([k, v]) => (
          <div className="setting" key={k}><span>{k}</span><span className="selectable" style={{ color: 'var(--ink-2)', textAlign: 'right', overflowWrap: 'anywhere' }}>{v}</span></div>
        ))}
      </Card>
    </div>
  );
}

function SetupSection({ onOpenSetup }: { onOpenSetup: () => void }) {
  return (
    <div className="set-section">
      <Card flush className="group-box">
        <div className="setting"><span>First-run setup<small style={{ display: 'block', color: 'var(--ink-3)', fontSize: 11 }}>Library folder, hardware, keys and local models</small></span><Button type="button" onClick={onOpenSetup}>Re-open setup…</Button></div>
      </Card>
    </div>
  );
}

/** The same documents the onboarding gate lists (LEGAL_DOCUMENT_SOURCES), each opened in the app. */
function LegalSection({ onView }: { onView: (id: LegalDocumentId) => void }) {
  return (
    <div className="set-section">
      <p>The agreement and notices you accepted at setup, and the notices for third-party software in this app. They open here in the app; nothing is fetched.</p>
      <Card flush className="group-box">
        {LEGAL_DOCUMENT_SOURCES.map((d) => (
          <div className="setting" key={d.id}>
            <span>{d.title}</span>
            <Button type="button" aria-label={`Read ${d.title}`} onClick={() => onView(d.id)}>Read</Button>
          </div>
        ))}
        <div className="setting" key={THIRD_PARTY_NOTICES.id}>
          <span>{THIRD_PARTY_NOTICES.title}</span>
          <Button type="button" aria-label={`Read ${THIRD_PARTY_NOTICES.title}`} onClick={() => onView(THIRD_PARTY_NOTICES.id)}>Read</Button>
        </div>
      </Card>
    </div>
  );
}

function AppsPanel() {
  const [apps, setApps] = useState<InstalledApp[]>([]);
  useEffect(() => { fetchApps().then(setApps, () => setApps([])); }, []);
  return (
    <div className="set-section">
      <h3>Orthic Labs apps on this computer</h3>
      <Card flush className="group-box">
        {apps.length === 0 ? <div className="setting"><span style={{ color: 'var(--ink-3)' }}>None found</span></div> : apps.map((a) => (
          <div className="setting" key={a.id ?? a.name}><span>{a.name}</span><span style={{ color: 'var(--ink-2)' }}>{a.version ?? ''}</span></div>
        ))}
      </Card>
    </div>
  );
}

/** Opens on `initialSection` when the section exists, else on the default (API keys). */
export function Settings({ onOpenSetup, initialSection }: { onOpenSetup: () => void; initialSection?: SettingsSectionId }) {
  const [viewing, setViewing] = useState<LegalDocumentId | null>(null);
  const sections = useMemo<SettingsSection[]>(() => mergeSections(
    // The licence section runs on @rightkit/license (Free, Pro, 30-day Pro trial); the engine does the
    // enforcing. No logs client exists in the host app yet, so that built-in section is not shown. Legal
    // documents are listed below.
    standardSections({
      appearance: { themeStore },
      license: { client: licenseClient, tierLabels: { pro: 'Pro' }, planSlot: <LicensePlan /> },
      updates: { client: updateClient, currentVersion: pkg.version, onRestart: () => relaunch() },
      about: { appName: hostAppName(), version: pkg.version, platform: platformOS, panels: <AppsPanel /> },
    }),
    [
      { id: 'setup', label: 'Setup', group: 'General', order: 200, keywords: ['first-run', 'onboarding', 'wizard'], render: () => <SetupSection onOpenSetup={onOpenSetup} /> },
      { id: 'legal', label: 'Legal', group: 'General', order: 250, keywords: ['license', 'eula', 'terms', 'privacy', 'agreement', 'acceptable use', 'product schedule', 'notices', 'third-party'], render: () => <LegalSection onView={setViewing} /> },
      { id: 'api-keys', label: 'API keys', group: 'Providers', order: 300, icon: sectionIcons.apiKeys, keywords: ['keychain', 'provider', 'balance', 'environment'], render: () => <KeysSection /> },
      { id: 'models', label: 'Models', group: 'Providers', order: 400, icon: sectionIcons.models, keywords: ['local', 'download', 'worker', 'catalog'], render: () => <ModelsPanel /> },
      { id: 'spend', label: 'Spend', group: 'Providers', order: 500, keywords: ['cap', 'budget', 'daily'], render: () => <SpendSection /> },
      { id: 'engine', label: 'Engine', group: 'Support', order: 800, icon: sectionIcons.diagnostics, keywords: ['doctor', 'health', 'status'], render: () => <EngineSection /> },
    ]
  ), [onOpenSetup]);
  const startId = initialSection && sections.some((s) => s.id === initialSection) ? initialSection : 'api-keys';
  return (
    <>
      <SettingsPanel backend={settingsBackend} sections={sections} title="Settings" defaultActiveId={startId} />
      {viewing ? <LegalDocumentView id={viewing} onClose={() => setViewing(null)} /> : null}
    </>
  );
}
