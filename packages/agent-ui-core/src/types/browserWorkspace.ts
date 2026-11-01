// BrowserWorkspace types — owned by the D3 workspace fragment.
// Mirrors the Rust `api_browser_workspace` OpenAPI component shapes so the
// frontend and daemon agree on camelCase JSON without a generated client
// (the D3 packet forbids generated-client work).

export type BrowserProfileKind = "ephemeral" | "named_shared";

export type HostPolicyDecision = "allow" | "ask" | "block";

export type BrowserActionActor = "agent" | "human" | "workspace";

export interface ViewportInfo {
  width: number;
  height: number;
  deviceScaleFactor: number;
}

export interface BrowserActionEntry {
  atMs: number;
  actor: BrowserActionActor;
  kind: string;
  detail: string;
}

export interface BrowserAnnotation {
  atMs: number;
  screenshotBase64?: string;
  locator?: string;
  viewport?: ViewportInfo;
  note: string;
}

export interface StoredAnnotation extends BrowserAnnotation {
  id: string;
  dispatched: boolean;
}

export interface BrowserWorkspaceSummary {
  paused: boolean;
  profileKind: BrowserProfileKind;
  profileLabel: string | null;
  viewport: ViewportInfo | null;
  actionLogLen: number;
  sessions: BrowserSessionSummary[];
}

export interface BrowserSessionSummary {
  sessionId: string;
  ownerSessionId: string;
  mode: "headless" | "visible";
  profile: {
    kind: string;
    label: string | null;
    ephemeral: boolean;
  };
  controlLease: {
    owner: string | null;
    generation: number;
  };
  url: string;
  title: string;
  consoleErrorCount: number;
  zoom: number | null;
}

export interface BrowserWorkspaceError {
  code:
    | "ssrf_refused"
    | "host_policy_blocked"
    | "browser_paused"
    | "session_not_found"
    | "consent_required"
    | "capability_denied";
  message: string;
}

export interface ScreenshotEnvelope {
  sessionId: string;
  url: string;
  capturedAtMs: number;
  mime: string;
  width: number;
  height: number;
  bytes: number;
  dataBase64: string;
  untrustedWrapped: string;
}

export interface AnnotationSent {
  annotationId: string;
  dispatchedToSession: string;
  acceptedAtMs: number;
}

export interface DevServer {
  port: number;
  url: string;
}

export interface ConsoleLine {
  level: string;
  text: string;
}
