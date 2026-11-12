// Apple-device Pro continuity: the renderer half of the native Pro bridge (the Rust half is
// `rightkit_tauri_shell::host::pro_bridge`). Typed calls over the app's native commands; the bridge
// token never reaches this layer, and there is no "sync now" (publish and consume are driven by
// launch, activation and the opt-in toggle).
import type { LicenseInvoke, LicenseView } from './index.js';

export type ProResolutionStatus = 'off' | 'blocked_no_keychain' | 'ok';
/** The native status (camelCase over the wire). `syncEnabled` is the pre-split Keychain flag. */
export interface ProBridgeStatus {
  syncEnabled?: boolean;
  keychainSyncEnabled: boolean;
  proResolutionEnabled: boolean;
  keychainReady: boolean;
  keychainDetail?: string | null;
  /** True while this device must not consume the shared bridge. */
  consumptionSuppressed: boolean;
  /** `blocked_no_keychain` is its own failure mode, never plain `off`. */
  proResolutionStatus: ProResolutionStatus;
  /** Reported only by a host that answers on platforms without iCloud Keychain. */
  supported?: boolean;
}
export interface ProBridgeCommands {
  status: string;
  setSyncEnabled: string;
  suppressConsumption: string;
  afterActivation: string;
}
export const PRO_BRIDGE_COMMANDS: Readonly<ProBridgeCommands> = Object.freeze({
  status: 'pro_bridge_status',
  setSyncEnabled: 'pro_bridge_set_sync_enabled',
  suppressConsumption: 'pro_bridge_suppress_consumption',
  afterActivation: 'pro_bridge_after_activation',
});
export interface ProBridgeConfig { invoke: LicenseInvoke; commands?: Partial<ProBridgeCommands> }

export function createProBridgeClient({ invoke, commands }: ProBridgeConfig) {
  const names: ProBridgeCommands = { ...PRO_BRIDGE_COMMANDS, ...commands };
  /** Best effort: a failure (an inert or absent command outside macOS) never blocks a licence operation. */
  async function suppressConsumption(suppressed: boolean): Promise<void> {
    try { await invoke(names.suppressConsumption, { suppressed }); } catch { /* best effort */ }
  }
  return {
    status: (): Promise<ProBridgeStatus> => invoke<ProBridgeStatus>(names.status),
    /** Turn continuity on or off for this device. On is an explicit reconnect (clears suppression); off never deletes the shared item. */
    async setSyncEnabled(enabled: boolean): Promise<void> { await invoke(names.setSyncEnabled, { enabled }); },
    suppressConsumption,
    /** The rule, before a deactivation: suppress first, so a launch cannot re-activate the seat that is being freed. */
    beforeDeactivate: (): Promise<void> => suppressConsumption(true),
    /** The rule, after an activation: clear the suppression only when the activation succeeded (`view.status === 'active'`). */
    async afterActivate(view: Pick<LicenseView, 'status'>): Promise<void> {
      if (view.status === 'active') await suppressConsumption(false);
    },
    /** For a host whose native side publishes after an explicit activation. Best effort. */
    async notifyAfterActivation(): Promise<void> {
      try { await invoke(names.afterActivation); } catch { /* best effort */ }
    },
  };
}
export type ProBridgeClient = ReturnType<typeof createProBridgeClient>;
