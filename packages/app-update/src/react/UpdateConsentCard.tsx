import { useEffect, useId, useRef, type ReactElement } from 'react';
import type { UpdateConsentAction, UpdateConsentState } from '../overlay.js';

export interface UpdateConsentCardProps {
  state: UpdateConsentState;
  /** Version shown in the question, when known. */
  version?: string;
  /** "Allow Once" for the action shown; the controller runs exactly that operation. */
  onAllow: (action: UpdateConsentAction) => void;
  /** "Don't Allow" and Escape; clears the card and runs nothing. */
  onDeny: () => void;
}

const questions: Record<UpdateConsentAction, { allow: string; ask: string }> = {
  download: { allow: 'Allow download once', ask: 'Download this update?' },
  install: { allow: 'Allow install once', ask: 'Install this update?' },
};

/**
 * Per-action approval card. It renders only while an approval is pending or has failed. The Allow button takes
 * focus when a request opens, Escape denies, and buttons disable while the approved operation runs.
 */
export function UpdateConsentCard({ state, version, onAllow, onDeny }: UpdateConsentCardProps): ReactElement | null {
  const titleId = useId();
  const messageId = useId();
  const allowRef = useRef<HTMLButtonElement>(null);
  const action = state.pending;

  useEffect(() => {
    if (action && !state.busy) allowRef.current?.focus();
  }, [action, state.busy]);

  if (!action && !state.error) return null;
  const versionText = version ? ` ${version}` : '';

  return (
    <div
      role="alertdialog"
      aria-labelledby={titleId}
      aria-describedby={messageId}
      aria-busy={state.busy}
      className="update-consent"
      onKeyDown={(event) => {
        if (event.key === 'Escape' && !state.busy) onDeny();
      }}
    >
      <h2 id={titleId}>{action ? questions[action].ask : 'Update failed'}</h2>
      <p id={messageId}>
        {action
          ? `Approve this one step${versionText ? ` for version${versionText}` : ''}. Approving one step does not approve the other.`
          : state.error}
      </p>
      {state.error && action ? <p role="alert">{state.error}</p> : null}
      {action ? (
        <div role="group" aria-label="Approval">
          <button ref={allowRef} type="button" disabled={state.busy} onClick={() => onAllow(action)}>
            {questions[action].allow}
          </button>
          <button type="button" disabled={state.busy} onClick={onDeny}>
            Don&apos;t allow
          </button>
        </div>
      ) : null}
    </div>
  );
}
