import { createLicenseClient } from './index.js';
import type { LicenseConfig, LicenseView, LicenseUnlisten } from './index.js';

export const RIGHTKIT_LICENSE_TESTING_DO_NOT_SHIP = 'RIGHTKIT_LICENSE_TESTING_DO_NOT_SHIP';
/** Browser QA only. No persistence, credentials, dates-based authority or network. */
export function createLicenseMock(initial?: LicenseView) {
  let view: LicenseView = initial ?? {
    mode: 'configured', status: 'none', lifecycle: 'unlicensed', tier: null, features: [],
    kind: null, stage: null, validUntil: null, confirmationDueAt: null, staleSince: null,
    licenseIdSuffix: null, busy: false, unresolved: false, revisionState: 'none', message: 'none',
  };
  const listeners = new Set<(event: { payload: unknown }) => void>();
  const clone = () => structuredClone(view);
  const notify = () => { for (const listener of listeners) listener({ payload: { view: clone() } }); };
  const bridge: LicenseConfig = {
    async invoke<T>(command: string, args?: Record<string, unknown>): Promise<T> {
      let result: unknown;
      switch (command) {
        case 'rightkit_license_status': result = clone(); break;
        case 'rightkit_license_activate':
          if (typeof args?.key !== 'string' || !args.key) throw { code: 'rejected', reason: 'keyNotAccepted', message: 'keyNotAccepted' };
          view = { mode: 'configured', status: 'active', lifecycle: 'active', tier: 'pro', features: [], kind: 'term', stage: 'time_bound', validUntil: null, confirmationDueAt: null, staleSince: null, licenseIdSuffix: 'MOCK', busy: false, unresolved: false, revisionState: 'active', message: 'none' };
          result = clone(); notify(); break;
        case 'rightkit_license_refresh': result = { view: clone(), outcome: view.revisionState === 'active' ? 'refreshed' : 'noCredential' }; break;
        case 'rightkit_license_deactivate':
          view = { mode: 'configured', status: 'none', lifecycle: 'unlicensed', tier: null, features: [], kind: null, stage: null, validUntil: null, confirmationDueAt: null, staleSince: null, licenseIdSuffix: null, busy: false, unresolved: false, revisionState: 'tombstone', message: 'none' };
          result = clone(); notify(); break;
        case 'rightkit_license_request_trial': result = { accepted: typeof args?.email === 'string' && args.email.includes('@'), message: typeof args?.email === 'string' && args.email.includes('@') ? 'trialRequested' : 'trialRefused' }; break;
        default: throw { code: 'unsupported', message: 'unsupported' };
      }
      return result as T;
    },
    async listen<T>(event: string, callback: (event: { payload: T }) => void): Promise<LicenseUnlisten> {
      if (event !== 'rightkit://license-changed') throw { code: 'unsupported', message: 'unsupported' };
      const listener = (event: { payload: unknown }) => callback(event as { payload: T });
      listeners.add(listener);
      return () => { listeners.delete(listener); };
    },
  };
  // Tag executed mock transport for final-artifact sentinel scanning.
  Object.defineProperty(bridge.invoke, RIGHTKIT_LICENSE_TESTING_DO_NOT_SHIP, { value: true });
  return { client: createLicenseClient(bridge), bridge, sentinel: RIGHTKIT_LICENSE_TESTING_DO_NOT_SHIP };
}
