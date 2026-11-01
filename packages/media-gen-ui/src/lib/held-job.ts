// Wording and actions for a job the engine holds for the user (status `outcome_unknown`).
// One source for the Jobs view, the job card and the voice batch view. No imports, so node tests load it.
//
// Two kinds of held job share the status:
//  - a paid submit that may have reached the provider: resubmit (nothing ran) or abandon (it ran, or may have);
//  - a paid result that arrived but could not be stored (error code `result_recovered`): adopt (store the
//    saved file, no provider call) or abandon. Resubmit is never offered for it.

export type HeldAction = 'resubmit' | 'abandon' | 'adopt';

/** Engine error code of a paid result that was received and saved but not yet stored in the library. */
export const RECOVERED_CODE = 'result_recovered';

export const isRecoveredCode = (code: string | null | undefined): boolean => code === RECOVERED_CODE;

export function heldLabel(recovered: boolean): string {
  return recovered ? 'Result saved, not stored' : 'Outcome unknown';
}

export function heldExplanation(provider: string, recovered: boolean): string {
  const name = provider || 'the provider';
  return recovered
    ? `${name} returned this result and charged for it, but it could not be stored in your library. The audio is saved safely on this computer and will not be requested again. Store it to finish the job, or abandon it.`
    : `Outcome unknown — this request may already be running at ${name}, so it was not sent again. The amount reserved for it stays counted. Check the provider's dashboard, then resubmit (nothing ran) or abandon (it ran, or may have).`;
}

export function heldActions(recovered: boolean): HeldAction[] {
  return recovered ? ['adopt', 'abandon'] : ['resubmit', 'abandon'];
}

export function heldActionLabel(action: HeldAction): string {
  return action === 'resubmit' ? 'Resubmit' : action === 'adopt' ? 'Store result' : 'Abandon';
}

export function heldConsequence(action: HeldAction): string {
  if (action === 'resubmit') {
    return 'Resubmit sends the request to the provider again and may be charged a second time if the first one actually ran. Continue?';
  }
  if (action === 'adopt') {
    return 'Store result adds the saved file to your library and finishes the job. Nothing is sent to the provider. Continue?';
  }
  return 'Abandon ends this job as failed and keeps its reserved amount counted as spent, because the provider may have charged for it. Continue?';
}

// ---------------------------------------------------------------------------
// A held character training (studio.characters.train_status with `resolve`).
// The engine never retries it. Only the user resolves it: `release` when nothing started at the
// provider (the reservation is returned), `abandon` when it ran or may have (the amount stays spent).
// ---------------------------------------------------------------------------

export type TrainingResolve = 'release' | 'abandon';

export const trainingHeldActions = (): TrainingResolve[] => ['release', 'abandon'];

export function trainingHeldActionLabel(action: TrainingResolve): string {
  return action === 'release' ? 'Nothing started: release the reservation' : 'It ran or may have: keep it counted as spent';
}

/** Dollar text for a reserved amount, or "unknown" when the engine sent none. */
export function trainingAmount(reservedUsd: number | null | undefined): string {
  return typeof reservedUsd === 'number' && Number.isFinite(reservedUsd) ? `$${reservedUsd.toFixed(2)}` : 'unknown';
}

export function trainingHeldExplanation(provider: string, reservedUsd: number | null | undefined): string {
  const name = provider || 'fal';
  return `${name} may have accepted this training, so it was not sent again. The reserved amount (${trainingAmount(reservedUsd)}) stays reserved until you resolve it. Check your ${name} dashboard, then release the reservation (nothing started) or keep it counted as spent (it ran, or may have).`;
}

/** "Held since <date and time>" from the engine's `held_at`, or '' when it is missing or unreadable. */
export function trainingHeldSince(heldAt: string | null | undefined): string {
  const time = heldAt ? Date.parse(heldAt) : Number.NaN;
  if (Number.isNaN(time)) return '';
  return `Held since ${new Date(time).toLocaleString('en-GB', { day: 'numeric', month: 'short', year: 'numeric', hour: '2-digit', minute: '2-digit' })}`;
}

export function trainingHeldTitle(action: TrainingResolve): string {
  return action === 'release' ? 'Release the reservation?' : 'Keep the amount counted as spent?';
}

export function trainingHeldConfirmLabel(action: TrainingResolve): string {
  return action === 'release' ? 'Release reservation' : 'Keep counted as spent';
}

export function trainingHeldConsequence(action: TrainingResolve, reservedUsd: number | null | undefined): string {
  const amount = trainingAmount(reservedUsd);
  if (action === 'release') {
    return `Release returns the ${amount} reservation to your budget and ends this training as failed. Choose it only if fal shows that no training started. If one did start, it is charged and the amount is not returned. Continue?`;
  }
  return `Keep counted as spent ends this training as failed and leaves ${amount} counted as spent, because fal may have charged for it. Continue?`;
}
