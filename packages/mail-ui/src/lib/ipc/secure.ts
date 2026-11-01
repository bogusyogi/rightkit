import { invoke } from "./invoke.js";

// The licence record and the device identity have no renderer command any more: the licence is the
// native client's (RightKit M12, `rightkit_license_*`, src/lib/license.ts) and the credentials never
// leave the OS credential store. What remains here is the small system-browser and alias surface.

/** Open a web/mailto URL in the system browser (Unsubscribe links, etc.). */
export function openExternal(url: string): Promise<void> {
  return invoke<void>("open_external", { url });
}

export interface SendAsInfo {
  email: string;
  name: string;
  is_default: boolean;
}

/** The account's send-as identities (primary + verified aliases). */
export function listAliases(accountId: string): Promise<SendAsInfo[]> {
  return invoke<SendAsInfo[]>("list_aliases", { accountId });
}
