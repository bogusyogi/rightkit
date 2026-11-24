// What a compact update-status row offers for an `AutoUpdateStatus`: which single action (if any), whether
// the row is busy, and what a screen reader should be told. Pure and framework-free; imports only types.
// The labels and icons are the app's: this returns kinds and parameters, never wording.
// Moved from HeardRight `components/SidebarUpdateStatus.tsx`.
import type { AutoUpdateStatus } from './index.js';

export type StatusActionKind = 'upgrade' | 'install' | 'restart' | 'retry';

export interface StatusAction {
  kind: StatusActionKind;
  /** The version the action concerns (`upgrade` and `install`). */
  version?: string;
  /** The action is the primary one for the row (install, restart). */
  accent: boolean;
}

/** The one action a status offers, or `null` while there is nothing to do (idle, checking, up to date, downloading). */
export function statusAction(status: AutoUpdateStatus): StatusAction | null {
  if (status.kind === 'available') {
    if (status.reason === 'pro_required') return { kind: 'upgrade', version: status.version, accent: false };
    return { kind: 'install', version: status.version, accent: true };
  }
  if (status.kind === 'deferred') return { kind: 'install', version: status.version, accent: true };
  if (status.kind === 'ready') return { kind: 'restart', version: status.version, accent: true };
  if (status.kind === 'error') return { kind: 'retry', accent: false };
  return null;
}

/** True while a check or a download runs (the row shows a spinner instead of a button). */
export function isStatusBusy(status: AutoUpdateStatus): boolean {
  return status.kind === 'checking' || status.kind === 'downloading';
}

export type StatusAnnouncement =
  | { kind: 'checking' }
  | { kind: 'downloading'; version: string; pct: number }
  | { kind: 'ready'; version: string }
  | { kind: 'error' }
  | { kind: 'available'; version: string }
  | { kind: 'up_to_date' }
  | { kind: 'idle' };

/** What to announce for a status; `idle` and `up_to_date` are not announced by a live region. */
export function statusAnnouncement(status: AutoUpdateStatus): StatusAnnouncement {
  switch (status.kind) {
    case 'checking':
      return { kind: 'checking' };
    case 'downloading':
      return { kind: 'downloading', version: status.version, pct: status.pct };
    case 'ready':
      return { kind: 'ready', version: status.version };
    case 'error':
      return { kind: 'error' };
    case 'available':
    case 'deferred':
      return { kind: 'available', version: status.version };
    case 'up_to_date':
      return { kind: 'up_to_date' };
    default:
      return { kind: 'idle' };
  }
}
