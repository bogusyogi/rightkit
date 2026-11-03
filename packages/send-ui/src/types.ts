// Wire types of rightkit-send's JSON command surface (`rightkit_send::commands`). Field names are
// what the crate serializes: the snapshot types are camelCase, `TrustEntry` keeps its stored
// snake_case layout.

export type TrustState = "allow" | "ask" | "deny";

/** A typed error from the crate: `code` is a snake_case tag, the other fields depend on it. */
export interface SendError {
  code: string;
  [detail: string]: unknown;
}

export interface SendDevice {
  fingerprint: string;
  alias: string;
  deviceModel: string | null;
  deviceType: string | null;
  ip: string;
  port: number;
  protocol: string;
  px: boolean;
}

export interface IncomingFile {
  name: string;
  size: number;
}

export interface Incoming {
  id: string;
  from: string;
  deviceModel: string | null;
  fingerprint: string;
  ip: string;
  fileCount: number;
  totalBytes: number;
  isMessage: boolean;
  preview: string | null;
  files: IncomingFile[];
  /** The sender proved its key by certificate. */
  verified: boolean;
}

export type TransferState = "waiting" | "active" | "done" | "failed" | "cancelled" | "declined";

export interface Transfer {
  id: string;
  direction: "send" | "receive";
  peer: string;
  peerFingerprint: string;
  state: TransferState;
  totalBytes: number;
  doneBytes: number;
  filesTotal: number;
  filesDone: number;
  current: string | null;
  accepted: boolean;
  savedTo: string | null;
  savedFiles: string[];
  error: SendError | null;
  message?: string;
  clipboard: boolean;
  started: number;
  finished: number | null;
}

export interface SendSnapshot {
  product: string;
  alias: string;
  fingerprint: string;
  port: number;
  saveDir: string;
  devices: SendDevice[];
  incoming: Incoming[];
  transfers: Transfer[];
  warnings: SendError[];
  /** "unknown", "granted" or "blocked" (macOS Local Network access). */
  localNetwork: "unknown" | "granted" | "blocked";
  scanning: boolean;
}

export interface TrustEntry {
  fingerprint: string;
  label: string;
  alias_seen: string;
  alias_claimed: string;
  model: string | null;
  kind: string | null;
  first_seen_ms: number;
  last_seen_ms: number;
  state: TrustState;
  proof: "certificate" | "callback" | null;
  decided: boolean;
  refused: number;
}

export interface SendRequest {
  /** A device's fingerprint or alias. */
  target: string;
  /** Absolute paths of files and folders. */
  paths?: string[];
  /** Text messages. */
  texts?: string[];
  /** The receiver puts the text on its clipboard (honoured only by an Allow device). */
  clipboard?: boolean;
}

/** How the UI calls the host: one command name and a JSON object of arguments. */
export type SendInvoke = (command: string, args?: Record<string, unknown>) => Promise<unknown>;
