import type {
  SendError,
  SendInvoke,
  SendRequest,
  SendSnapshot,
  TrustEntry,
  TrustState,
} from "./types.js";

/** The typed calls of `rightkit_send::commands::invoke`. */
export interface SendClient {
  snapshot(): Promise<SendSnapshot>;
  refresh(): Promise<void>;
  /** Starts a send and returns its transfer id; progress arrives in later snapshots. */
  send(request: SendRequest): Promise<string>;
  /** Answers a waiting request. Resolves false when it is no longer waiting. */
  respond(id: string, accept: boolean): Promise<boolean>;
  cancel(id: string): Promise<boolean>;
  dismiss(id: string): Promise<void>;
  setSaveDir(path: string): Promise<void>;
  trustList(): Promise<TrustEntry[]>;
  trustDefault(): Promise<TrustState>;
  trustSet(fingerprint: string, state: TrustState): Promise<void>;
  trustLabel(fingerprint: string, label: string): Promise<void>;
  trustForget(fingerprint: string): Promise<void>;
  trustSetDefault(state: Exclude<TrustState, "allow">): Promise<void>;
}

/**
 * Builds the client over the host's one command. For Tauri:
 *
 * ```ts
 * import { invoke } from "@tauri-apps/api/core";
 * const client = createSendClient((command, args) => invoke("send_invoke", { command, args }));
 * ```
 */
export function createSendClient(invoke: SendInvoke): SendClient {
  const call = <T>(command: string, args?: Record<string, unknown>) =>
    invoke(command, args) as Promise<T>;
  const none = async (command: string, args?: Record<string, unknown>) => {
    await invoke(command, args);
  };
  return {
    snapshot: () => call<SendSnapshot>("snapshot"),
    refresh: () => none("refresh"),
    send: (request) => call<string>("send", { ...request }),
    respond: (id, accept) => call<boolean>("respond", { id, accept }),
    cancel: (id) => call<boolean>("cancel", { id }),
    dismiss: (id) => none("dismiss", { id }),
    setSaveDir: (path) => none("setSaveDir", { path }),
    trustList: () => call<TrustEntry[]>("trustList"),
    trustDefault: () => call<TrustState>("trustDefault"),
    trustSet: (fingerprint, state) => none("trustSet", { fingerprint, state }),
    trustLabel: (fingerprint, label) => none("trustLabel", { fingerprint, label }),
    trustForget: (fingerprint) => none("trustForget", { fingerprint }),
    trustSetDefault: (state) => none("trustSetDefault", { state }),
  };
}

/** Whether a thrown value is the crate's typed error. */
export function isSendError(value: unknown): value is SendError {
  return (
    typeof value === "object" &&
    value !== null &&
    typeof (value as { code?: unknown }).code === "string"
  );
}
