import { useEffect, useRef, type ReactElement } from 'react';
import type { UpdateOverlayModel } from '../overlay.js';

export interface UpdateOverlayProps {
  model: UpdateOverlayModel;
  onCheck?: () => void;
  onDownload?: () => void;
  onInstall?: () => void;
  onDismiss?: () => void;
}

/** Plain-language status text for a view model; the package keeps copy out of the framework-free model. */
export function updateOverlayText(model: UpdateOverlayModel): { title: string; detail?: string } {
  const version = model.version ? ` ${model.version}` : '';
  switch (model.phase) {
    case 'hidden':
      return { title: '' };
    case 'checking':
      return { title: 'Checking for updates' };
    case 'up_to_date':
      return { title: 'You are up to date' };
    case 'available':
      return { title: `Update${version} is available` };
    case 'available_ineligible':
      return {
        title: `Update${version} is available`,
        detail: model.reason === 'pro_required' ? 'This update requires a Pro plan.' : 'Automatic installs are off.',
      };
    case 'deferred':
      return { title: `Update${version} is waiting`, detail: model.reason ? `Install paused: ${model.reason}.` : undefined };
    case 'downloading':
      return { title: `Downloading update${version}`, detail: model.progress === undefined ? undefined : `${model.progress}%` };
    case 'staged':
      return { title: `Update${version} is ready to install` };
    case 'ready':
      return { title: `Update${version} is installed`, detail: 'Restart to finish.' };
    case 'error':
      return { title: 'Update failed', detail: model.message };
  }
}

/**
 * Renders an update overlay from the framework-free view model. Buttons appear only for the actions the model
 * offers. The region takes focus when the phase changes to a settled state so screen readers announce it.
 */
export function UpdateOverlay({ model, onCheck, onDownload, onInstall, onDismiss }: UpdateOverlayProps): ReactElement | null {
  const rootRef = useRef<HTMLElement>(null);
  useEffect(() => {
    if (model.phase === 'hidden' || model.phase === 'checking' || model.phase === 'downloading') return;
    rootRef.current?.focus();
  }, [model.phase]);

  if (model.phase === 'hidden') return null;
  const { title, detail } = updateOverlayText(model);
  const { actions } = model;
  return (
    <section ref={rootRef} tabIndex={-1} aria-label="Software update" className="update-overlay" data-phase={model.phase}>
      <p role="status" aria-live="polite">
        <strong>{title}</strong>
        {detail ? <span> {detail}</span> : null}
      </p>
      {model.phase === 'downloading' && model.progress !== undefined ? (
        <progress
          aria-label="Download progress"
          max={100}
          value={model.progress}
        />
      ) : null}
      <div role="group" aria-label="Update actions">
        {actions.check && onCheck ? <button type="button" onClick={onCheck}>Check for updates</button> : null}
        {actions.download && onDownload ? <button type="button" onClick={onDownload}>Download update</button> : null}
        {actions.install && onInstall ? <button type="button" onClick={onInstall}>Install update</button> : null}
        {actions.dismiss && onDismiss ? <button type="button" onClick={onDismiss}>Dismiss</button> : null}
      </div>
    </section>
  );
}
