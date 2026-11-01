// Agent demo mode: TypeScript bindings.
//
// Two layers, both derived from Rust:
//  1. The agent wire contract (`demo.*` tools). The source of truth is rightkit-agent-tools
//     (`src/backends/demo.rs`, `docs/demo-contract.md`). These types mirror it field for field; the UI
//     never sends them, but the shapes are kept here so the contract has one TypeScript home.
//  2. The Tauri commands and events of the the host app host (`src-tauri/src/demo/`).
//
// Nothing here carries the session token: it never leaves the host.

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

// ---------------------------------------------------------------- wire contract (rightkit-agent-tools)

export type DemoErrorCode =
  | "demo_disabled"
  | "consent_denied"
  | "invalid_token"
  | "session_ended"
  | "session_killed"
  | "unknown_app"
  | "not_armed"
  | "permission_missing"
  | "target_blocked"
  | "capture_unsafe"
  | "invalid_arguments"
  | "service_unavailable"
  | "malformed_response";

export type ElementOp = "click" | "type" | "press";

export type WireStep = { ref: string; op: ElementOp; text?: string; key?: string };
export type WireApp = { id: string; name: string };
export type WireSnapshotElement = { ref: string; role: string; label: string; ops: ElementOp[] };
export type WireSnapshotResult = { capture_state: "safe"; elements: WireSnapshotElement[] };
export type WireBatchResult = {
  completed: number;
  results: { receipt_id: string }[];
  failed_step: number | null;
  error_code: DemoErrorCode | null;
};
export type WireEndResult = { ended: boolean; bundle_id: string; bundle_state: "complete" | "killed" };

export const DEMO_SERVICE = "rightkit.agent.demo";
export const MAX_BATCH_STEPS = 20;

// ---------------------------------------------------------------- host state and events

export type Phase = "idle" | "awaiting_consent" | "armed" | "recording" | "stopping" | "sealed" | "killed";

export type DemoSession = {
  id: string;
  kind: "agent" | "person";
  app: string;
  app_name: string;
  agent_name: string | null;
  started_ms: number | null;
};

export type DemoBundle = {
  /** Folder name; what reveal and discard take. */
  name: string;
  id: string;
  dir: string;
  state: "complete" | "killed" | "quarantined" | "invalid";
  verified: boolean;
  problems: string[];
  reason: string | null;
};

export type DemoState = {
  phase: Phase;
  session: DemoSession | null;
  bundle: DemoBundle | null;
  message: string | null;
  hotkey: string;
};

/** What the consent dialog shows. `agent_exe` and `agent_pid` come from the OS; `agent_name` is a claim. */
export type DemoConsent = {
  session: string;
  app: string;
  app_name: string;
  agent_name: string;
  agent_exe: string | null;
  agent_pid: number | null;
  will_do: string;
};

/** Content-free: no typed text, no element label, no value. */
export type DemoReceipt = {
  receipt_id: string;
  session: string;
  action: string;
  result: "done" | "blocked" | "failed";
  denied_by: string | null;
  tier: string | null;
  ts_ms: number;
};

export type DemoStatus = {
  compiled: boolean;
  enabled: boolean;
  hotkey: string;
  state: DemoState;
  pending_consent: DemoConsent | null;
};

export type DemoPrepare = {
  accessibility: boolean;
  screen_recording: boolean;
  recorder: string;
  notes: string[];
};

export type DemoCatalogItem = { id: string; name: string };

export const EVENT_STATE = "demo://state";
export const EVENT_CONSENT = "demo://consent";
export const EVENT_RECEIPT = "demo://receipt";

// ---------------------------------------------------------------- commands

export const demoStatus = () => invoke<DemoStatus>("demo_status");
export const demoSetEnabled = (enabled: boolean) => invoke<DemoStatus>("demo_set_enabled", { enabled });
export const demoCatalog = () => invoke<DemoCatalogItem[]>("demo_catalog");
export const demoPrepare = () => invoke<DemoPrepare>("demo_prepare");
export const demoConsentRespond = (session: string, approve: boolean) =>
  invoke<void>("demo_consent_respond", { session, approve });
export const demoBeginPerson = (app: string) => invoke<void>("demo_begin_person", { app });
export const demoStop = () => invoke<DemoBundle | null>("demo_stop");
export const demoKill = () => invoke<void>("demo_kill");
export const demoBundleReveal = (name: string) => invoke<void>("demo_bundle_reveal", { name });
export const demoBundleDiscard = (name: string) => invoke<void>("demo_bundle_discard", { name });

export const onDemoState = (f: (s: DemoState) => void): Promise<UnlistenFn> =>
  listen<DemoState>(EVENT_STATE, (e) => f(e.payload));
export const onDemoConsent = (f: (c: DemoConsent) => void): Promise<UnlistenFn> =>
  listen<DemoConsent>(EVENT_CONSENT, (e) => f(e.payload));
export const onDemoReceipt = (f: (r: DemoReceipt) => void): Promise<UnlistenFn> =>
  listen<DemoReceipt>(EVENT_RECEIPT, (e) => f(e.payload));

/** Tauri rejects with the command's `{code, message}`, or a string for a missing command. */
export function demoErrorMessage(e: unknown): string {
  if (typeof e === "string") return e;
  if (e && typeof e === "object" && "message" in e) return String((e as { message: unknown }).message);
  return "Something went wrong.";
}

/** One-line wording for a phase, shared by the badge and the settings page. */
export function phaseLabel(p: Phase): string {
  switch (p) {
    case "awaiting_consent":
      return "Waiting for your answer";
    case "armed":
      return "Ready, not recording yet";
    case "recording":
      return "Recording";
    case "stopping":
      return "Saving the recording";
    case "sealed":
      return "Recording saved";
    case "killed":
      return "Stopped by the kill control";
    default:
      return "Off";
  }
}
