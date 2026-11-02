import { onboardingStateKey, type LegalReceiptPort, type LegalStepConfig, type PersistedOnboardingState } from '@rightkit/onboarding';
import { createAcceptanceReceipt, needsLegalAcceptance } from '@rightkit/legal/browser';
import { LEGAL_AGREEMENT_STATEMENT, LEGAL_DOCUMENTS_HASH, LEGAL_DOCUMENT_SOURCES, LEGAL_MANIFEST, type LegalDocumentId } from '../legal/documents.js';
import { closeWindow } from './windowControls.js';
import { settingsBackend } from './settings-backend.js';
import { hostAppName } from "../appName.js";

/**
 * Version of the legal documents a user must have accepted: the @rightkit/legal manifest's
 * acceptance version. Text changes to an agreed document re-prompt through LEGAL_DOCUMENTS_HASH
 * (the material digest), which the gate passes as `requiredDocumentsHash`.
 */
/** Read after the host configured its legal manifest (configureLegal). */
export const legalVersion = (): string => LEGAL_MANIFEST.acceptanceVersion;

/** Settings key holding the full @rightkit/legal acceptance receipt. */
const LEGAL_RECEIPT_KEY = 'genright.legal.receipt';

async function readReceipt(): Promise<unknown> {
  try {
    return (await settingsBackend.load()).values[LEGAL_RECEIPT_KEY];
  } catch {
    return undefined;
  }
}

/**
 * Stores the acceptance as a @rightkit/legal receipt. `needsLegalAcceptance` decides freshness
 * against the manifest's material documents; a stale, malformed or missing receipt means none.
 */
export const legalReceiptPort: LegalReceiptPort = {
  async read() {
    const receipt = await readReceipt();
    if (needsLegalAcceptance(LEGAL_MANIFEST, receipt)) return null;
    const stored = receipt as { acceptedAt: string; eligibilityBasis: 'individual' | 'enterprise' };
    return { version: legalVersion(), documentsHash: LEGAL_DOCUMENTS_HASH, acceptedAt: stored.acceptedAt, eligibilityBasis: stored.eligibilityBasis };
  },
  async write(record) {
    const receipt = createAcceptanceReceipt(LEGAL_MANIFEST, { acceptedAt: record.acceptedAt, surface: 'first_run', eligibilityBasis: record.eligibilityBasis });
    await settingsBackend.patch({ [LEGAL_RECEIPT_KEY]: receipt });
  },
};

/** Id of the final setup step; completing it (or skipping) ends first-run setup. */
export const LAST_STEP_ID = 'models';


/**
 * The gate config. Documents are bundled (no hosted URLs exist yet), so each opens in the app
 * through `openDocument`. `lockup: null` replaces the legal-ui default publisher mark, which does
 * not describe the host app; the default trading-name line is kept.
 */
export function legalStepConfig(openDocument: (id: LegalDocumentId) => void): LegalStepConfig {
  return {
    appName: hostAppName(),
    trustLine: `${hostAppName()} generates images, video and voice on this computer or through the provider accounts you connect. Your keys stay in your system keychain.`,
    documents: LEGAL_DOCUMENT_SOURCES.map((d) => ({ title: d.title, onOpen: () => openDocument(d.id) })),
    agreementStatement: LEGAL_AGREEMENT_STATEMENT,
    lockup: null,
    // The suite gate is light-only: suite ivory base with the host app AA light accent (brand-action).
    theme: {
      bg: '#FBF7EF',
      surface: '#FFFCF5',
      border: '#E6DECF',
      text: '#231F1A',
      muted: '#6B655C',
      accent: '#7040C4',
      accentContrast: '#FFFFFF',
      fontDisplay: "Tanker, 'Arial Narrow', sans-serif",
      fontBody: "-apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif",
    },
    wordmarkFontUrl: '/fonts/Tanker-400.woff2',
    onQuit: () => closeWindow(),
  };
}

export type Startup = 'app' | 'setup';

async function readState(): Promise<Partial<PersistedOnboardingState> | undefined> {
  try {
    const snap = await settingsBackend.load();
    return snap.values[onboardingStateKey()] as Partial<PersistedOnboardingState> | undefined;
  } catch {
    return undefined;
  }
}

/**
 * What to show at launch.
 * - `app`: the legal agreement is accepted and setup was finished or skipped.
 * - `setup`: agreement and steps, resuming at the saved step.
 * QA (dev builds only): `?qaState=app` skips everything, `?qaState=onboarding` forces setup.
 */
export async function resolveStartup(): Promise<Startup> {
  if (import.meta.env?.DEV) {
    const qa = new URLSearchParams(window.location.search).get('qaState');
    if (qa === 'app') return 'app';
    if (qa === 'onboarding') return 'setup';
  }
  const state = await readState();
  const accepted = !needsLegalAcceptance(LEGAL_MANIFEST, await readReceipt());
  // The final step counts as handled when it was completed OR skipped with its per-step Skip button (`skippedSteps`):
  // a skipped last step is stored in `skippedSteps`, not `completed`, and must not reopen setup.
  const lastHandled = state?.completed?.includes(LAST_STEP_ID) === true || state?.skippedSteps?.includes(LAST_STEP_ID) === true;
  if (accepted && (state?.skipped === true || lastHandled)) return 'app';
  return 'setup';
}

/** Re-open setup from Settings: keep the accepted agreement, restart at the first step. */
export async function restartSetup(): Promise<void> {
  const state = await readState();
  if (!state) return;
  await settingsBackend.patch({ [onboardingStateKey()]: { ...state, currentStep: 1, skipped: false } });
}
