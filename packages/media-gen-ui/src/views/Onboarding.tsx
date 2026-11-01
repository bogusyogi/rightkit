import { Fragment, useEffect, useMemo, useState, type ReactNode } from 'react';
import { LEGAL_DOCUMENTS_HASH, type LegalDocumentId } from '../legal/documents.js';
import { Card, Badge, CaptionControls, Notice } from '@rightkit/react-shell/react';
import { Onboarding, modelsStep, type OnboardingStep } from '@rightkit/onboarding';
import { KeyRow } from '../components/KeyRow.js';
import { ImportEnvButton } from '../components/ImportEnvButton.js';
import { errorMessage, fetchApps, fetchCredentialStatus, fetchLocalImageRuntime, type CredentialRuntime, type InstalledApp } from '../lib/api.js';
import { useApp } from '../lib/app-state.js';
import { pick, scalarRows } from '../lib/doctor.js';
import { createLocalModelsBackend } from '../lib/local-models-backend.js';
import { LAST_STEP_ID, legalVersion, legalReceiptPort, legalStepConfig } from '../lib/onboarding.js';
import { isMac, platformOS } from '../lib/platform.js';
import { settingsBackend } from '../lib/settings-backend.js';
import { setupStepDef, type SetupStepFacts, type SetupStepId } from '../lib/setup-steps.js';
import type { LocalImageRuntimeStatus } from '../lib/types.js';
import { WINDOW_CONTROLS } from '../lib/windowControls.js';
import { LegalDocumentView } from './LegalDocument.js';
import { hostAppName } from "../appName.js";

function Row({ k, children }: { k: string; children: ReactNode }) {
  return <div className="setting"><span>{k}</span><span className="selectable" style={{ color: 'var(--ink-2)', textAlign: 'right', overflowWrap: 'anywhere' }}>{children}</span></div>;
}

function LibraryStep() {
  const app = useApp();
  const libraryDir = pick(app.doctor, 'library_dir', 'library', 'data_dir', 'data_root') ?? `${hostAppName()} data/library`;
  return (
    <>
      <Card flush className="group-box">
        <Row k="Library folder">{libraryDir}</Row>
        <Row k="Generated files">Saved inside the library, next to their metadata</Row>
      </Card>
      <p className="lede" style={{ marginTop: 14, fontSize: 12 }}>Nothing is uploaded during setup. You can come back from Settings → Setup.</p>
    </>
  );
}

function ComputerStep() {
  const app = useApp();
  const [runtime, setRuntime] = useState<LocalImageRuntimeStatus | null>(null);
  useEffect(() => { fetchLocalImageRuntime().then(setRuntime, () => setRuntime(null)); }, []);
  const profiles = app.localProfiles;
  const doctorRows = scalarRows(app.doctor).filter(([k]) => !['Ok', 'Library dir', 'Platform', 'Os', 'Arch', 'Architecture'].includes(k));
  const os = pick(app.doctor, 'platform.os', 'platform', 'os') ?? platformOS;
  const arch = pick(app.doctor, 'platform.arch', 'arch', 'architecture') ?? 'unknown';
  return (
    <>
      {app.doctorError ? <Notice tone="bad">{app.doctorError}</Notice> : null}
      <Card flush className="group-box">
        <Row k="Platform">{os}</Row>
        <Row k="Architecture">{arch}</Row>
        {doctorRows.slice(0, 5).map(([k, v]) => <Row key={k} k={k}>{v}</Row>)}
        <Row k="Engine"><Badge tone={app.doctorError ? 'bad' : app.doctor ? 'ok' : undefined}>{app.doctorError ? 'Problem' : app.doctor ? 'Healthy' : 'Checking…'}</Badge></Row>
        <Row k="Local image runtime">{runtime ? runtime.state.replace('_', ' ') : 'not reported'}</Row>
        <Row k="Local image models">{profiles.length ? `${profiles.filter((p) => p.install_state === 'installed').length} of ${profiles.length} installed` : 'none listed'}</Row>
      </Card>
    </>
  );
}

function InstalledStep() {
  const [apps, setApps] = useState<InstalledApp[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    fetchApps().then((list) => setApps(list.filter((a) => a.name !== hostAppName())), (e) => { setApps([]); setError(errorMessage(e)); });
  }, []);
  return (
    <>
      {error ? <Notice tone="bad">{error}</Notice> : null}
      <Card flush className="group-box">
        {apps === null ? <div className="setting"><span>Looking…</span></div> : null}
        {apps?.length === 0 && !error ? <div className="setting"><span style={{ color: 'var(--ink-3)' }}>No other Orthic Labs apps found.</span></div> : null}
        {apps?.map((a) => (
          <div className="setting" key={a.id ?? a.name}>
            <span><b style={{ fontWeight: 500 }}>{a.name}</b>{a.path ? <small className="mono selectable" style={{ display: 'block', color: 'var(--ink-3)' }}>{a.path}</small> : null}</span>
            <span style={{ color: 'var(--ink-2)' }}>{a.version ? `v${a.version}` : ''}</span>
          </div>
        ))}
      </Card>
    </>
  );
}

function KeysStep() {
  const app = useApp();
  const [providers, setProviders] = useState<Array<{ id: string; configured: boolean; needs_key?: boolean; runtime?: CredentialRuntime | null }>>([]);
  const [error, setError] = useState<string | null>(null);
  const load = () =>
    fetchCredentialStatus().then(
      (s) => { setProviders(s.providers); setError(null); void app.refreshCredentials(); },
      (e) => setError(errorMessage(e))
    );
  useEffect(() => { void load(); }, []); // eslint-disable-line react-hooks/exhaustive-deps
  return (
    <>
      {error ? <Notice tone="bad">{error}</Notice> : null}
      <ImportEnvButton onDone={() => void load()} />
      <Card flush className="group-box">
        {providers.map((p) => <KeyRow key={p.id} id={p.id} configured={p.configured} needsKey={p.needs_key !== false} runtime={p.runtime} compact onChanged={() => void load()} />)}
      </Card>
    </>
  );
}

function ModelsIntro() {
  return (
    <p>
      Image models can run on this computer. Voice and video use cloud providers through your API keys, and you will always see which route a job takes. Downloads continue in the background and each file is checked against its hash before it is activated.
    </p>
  );
}

/** The four facts every step states: purpose, cost, what skipping costs, and how to resume. */
function StepFacts({ facts }: { facts: SetupStepFacts }) {
  const items: Array<[string, string]> = [
    ['Purpose', facts.purpose],
    ['Cost', facts.cost],
    ['If skipped', facts.skipCost],
    ['Resume later', facts.resume],
  ];
  return (
    <Card flush className="group-box gb-mt0 gb-mb14">
    <dl className="ob-facts">
      {items.map(([k, v]) => (
        <Fragment key={k}>
          <dt style={{ color: 'var(--ink-3)' }}>{k}</dt>
          <dd style={{ margin: 0, color: 'var(--ink-2)' }}>{v}</dd>
        </Fragment>
      ))}
    </dl>
    </Card>
  );
}

/** One step of the shared definition, with its body rendered under the facts box. */
function setupStep(id: Exclude<SetupStepId, 'models'>, body: () => ReactNode, description?: string): OnboardingStep {
  const def = setupStepDef(id);
  return {
    id: def.id,
    label: def.label,
    title: def.title,
    description: description ?? def.description,
    skippable: def.skippable,
    render: () => <><StepFacts facts={def.facts} />{body()}</>,
  };
}

const KEYS_DESCRIPTION = `Keys are stored in your ${isMac ? 'macOS Keychain' : 'Windows Credential Manager'}, never in project files. Add only what you use, or skip and work locally.`;

const STEPS: OnboardingStep[] = [
  setupStep('library', () => <LibraryStep />),
  setupStep('computer', () => <ComputerStep />),
  setupStep('installed', () => <InstalledStep />),
  setupStep('keys', () => <KeysStep />, KEYS_DESCRIPTION),
];

/**
 * First-run setup on @rightkit/onboarding: the legal agreement gate, then the host app steps and the
 * local-model step. The model step is last, matching LAST_STEP_ID.
 */
export function Setup({ onDone }: { onDone: () => void }) {
  const { refreshLocalProfiles } = useApp();
  const [viewing, setViewing] = useState<LegalDocumentId | null>(null);
  const legal = useMemo(() => legalStepConfig(setViewing), []);
  const steps = useMemo(() => {
    const models = createLocalModelsBackend(() => void refreshLocalProfiles());
    const def = setupStepDef('models');
    const intro = <><ModelsIntro /><StepFacts facts={def.facts} /></>;
    return [...STEPS, { ...modelsStep(models, { label: def.label, title: def.title, intro }), id: LAST_STEP_ID }];
  }, [refreshLocalProfiles]);
  return (
    <>
      <Onboarding
        settings={settingsBackend}
        legal={legal}
        requiredVersion={legalVersion()}
        requiredDocumentsHash={LEGAL_DOCUMENTS_HASH}
        legalReceipt={legalReceiptPort}
        steps={steps}
        dragRegion
        titlebar={isMac ? undefined : <div style={{ marginLeft: 'auto', alignSelf: 'stretch', display: 'flex' }}><CaptionControls adapter={WINDOW_CONTROLS} /></div>}
        onComplete={onDone}
        onSkip={onDone}
      />
      {viewing ? <LegalDocumentView id={viewing} onClose={() => setViewing(null)} /> : null}
    </>
  );
}
