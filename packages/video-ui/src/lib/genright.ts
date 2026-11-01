// `genright.status` as the engine returns it (crates/cutright-engine/src/genright_cmd.rs `status`):
//   ready:    {installed:true, ready:true, action:null, engine_path, app_version, contract_version, error:null}
//   missing:  {installed:false, ready:false, action:"install", error:{code:"genright_not_installed"|"genright_unavailable", message}}
//             (installed but without an engine also comes back installed:false, action:"update")
//   other:    {installed:true, ready:false, action:"update"|"install", engine_path, error:{code, message}}
//             code "genright_outdated" carries `contract version <found> is not supported (need <required>)`
//             or `lacks capability <name> v<n>` in its message; anything else means the engine failed to run.

import { hostAppName } from "../appName";
export type HostState = "checking" | "ready" | "missing" | "outdated" | "unavailable";

export type HostInfo = {
  state: HostState;
  /** Engine error message, as reported. */
  detail?: string;
  enginePath?: string;
  appVersion?: string;
  /** Contract version the installed the host app reports (ready) or was found at (outdated). */
  found?: number;
  /** Contract version the host app requires (outdated, parsed from the engine message). */
  required?: number;
  /** What the user should do: install or update the host app. */
  action?: "install" | "update";
};

const isObj = (v: unknown): v is Record<string, unknown> => typeof v === "object" && v !== null;
const str = (v: unknown) => (typeof v === "string" ? v : undefined);
const num = (v: unknown) => (typeof v === "number" ? v : undefined);

export function interpretHost(result: unknown): HostInfo {
  const r = isObj(result) ? result : {};
  const err = isObj(r.error) ? r.error : undefined;
  const code = str(err?.code);
  const detail = str(err?.message);
  const enginePath = str(r.engine_path);
  const action = r.action === "install" || r.action === "update" ? r.action : undefined;
  if (r.ready === true) {
    return {
      state: "ready",
      enginePath,
      appVersion: str(r.app_version),
      found: num(r.contract_version),
    };
  }
  if (r.installed === false || code === "genright_not_installed") {
    return { state: "missing", detail, action: action ?? "install" };
  }
  if (code === "genright_outdated") {
    const m = /contract version (\d+) is not supported \(need (\d+)\)/.exec(detail ?? "");
    return {
      state: "outdated",
      detail,
      enginePath,
      found: m ? Number(m[1]) : undefined,
      required: m ? Number(m[2]) : undefined,
      action: "update",
    };
  }
  if (r.ready === false) {
    return { state: "unavailable", detail, enginePath, action: action ?? "update" };
  }
  return { state: "unavailable", detail: detail ?? `${hostAppName()} did not report ready.`, enginePath };
}

/** Status used when the call itself failed (engine down, timeout). */
export function genRightCallFailed(message: string): HostInfo {
  return { state: "unavailable", detail: message };
}

// ---- `genright.jobs` / `genright.recover` -------------------------------
// Request/job rows come from the project's the host app journal
// (crates/cutright-engine/src/genright_cmd.rs `request_row`).

export type HostJobState =
  | "queued"
  | "running"
  | "ready"
  | "failed"
  | "cancelled"
  | "done"
  | "unknown"
  | "import_failed";

export type HostJob = {
  requestId: string;
  kind: string;
  provider: string;
  model: string;
  prompt: string;
  jobId: string | null;
  /** Normalized: queued | running | ready | failed | cancelled | done. */
  state: HostJobState;
  /** the host app own status word when a live `job status` was fetched. */
  engineStatus: string | null;
  error: { code?: string; message?: string } | null;
  imported: boolean;
  estUsd: number | null;
  actualUsd: number | null;
};

const optStr = (v: unknown) => (typeof v === "string" && v.length > 0 ? v : null);

export function interpretJobRow(raw: unknown): HostJob | null {
  if (!isObj(raw)) return null;
  const requestId = str(raw.request_id);
  if (!requestId) return null;
  const err = isObj(raw.error) ? raw.error : null;
  const state = str(raw.state);
  return {
    requestId,
    kind: str(raw.kind) ?? "",
    provider: str(raw.provider) ?? "",
    model: str(raw.model) ?? "",
    prompt: str(raw.prompt) ?? "",
    jobId: optStr(raw.job_id),
    state: (state as HostJobState) ?? "unknown",
    engineStatus: optStr(raw.engine_status),
    error: err ? { code: str(err.code), message: str(err.message) } : null,
    imported: raw.imported === true,
    estUsd: num(raw.est_usd) ?? null,
    actualUsd: num(raw.actual_usd) ?? null,
  };
}

export function interpretJobs(result: unknown): HostJob[] {
  const rows = isObj(result) && Array.isArray(result.jobs) ? result.jobs : [];
  return rows.map(interpretJobRow).filter((j): j is HostJob => j !== null);
}

/** One entry of `genright.recover`'s `recovered` array. */
export type RecoveredRequest = {
  requestId: string;
  state: HostJobState;
  jobId?: string;
  imported?: number;
  error: { code?: string; message?: string } | null;
};

export function interpretRecover(result: unknown): RecoveredRequest[] {
  const rows = isObj(result) && Array.isArray(result.recovered) ? result.recovered : [];
  const out: RecoveredRequest[] = [];
  for (const raw of rows) {
    if (!isObj(raw)) continue;
    const requestId = str(raw.request_id);
    if (!requestId) continue;
    const err = isObj(raw.error) ? raw.error : null;
    out.push({
      requestId,
      state: (str(raw.state) as HostJobState) ?? "unknown",
      jobId: optStr(raw.job_id) ?? undefined,
      imported: num(raw.imported),
      error: err ? { code: str(err.code), message: str(err.message) } : null,
    });
  }
  return out;
}

// ---- `assets.list` -----------------------------------------------------
// One row of the imported-asset library (`assets_list`).

export type GeneratedAsset = {
  assetId: string;
  jobId: string;
  requestId: string | null;
  kind: string | null;
  path: string;
  mime: string;
  bytes: number;
  blake3: string;
  provider: string;
  model: string;
  prompt: string | null;
  estUsd: number | null;
  actualUsd: number | null;
  importedAt: number;
  /** true when the project copy is still on disk. */
  exists: boolean;
  /** ready | missing */
  state: string;
  /** relink | regenerate | null — what to offer for a missing file. */
  recovery: string | null;
  /** Timeline source id once registered (BLAKE3), else null. */
  sourceId: string | null;
};

export function interpretAsset(raw: unknown): GeneratedAsset | null {
  if (!isObj(raw)) return null;
  const assetId = str(raw.asset_id);
  const path = str(raw.path);
  if (!assetId || !path) return null;
  return {
    assetId,
    jobId: str(raw.job_id) ?? "",
    requestId: optStr(raw.request_id),
    kind: optStr(raw.kind),
    path,
    mime: str(raw.mime) ?? "",
    bytes: num(raw.bytes) ?? 0,
    blake3: str(raw.blake3) ?? "",
    provider: str(raw.provider) ?? "",
    model: str(raw.model) ?? "",
    prompt: optStr(raw.prompt),
    estUsd: num(raw.est_usd) ?? null,
    actualUsd: num(raw.actual_usd) ?? null,
    importedAt: num(raw.imported_at) ?? 0,
    exists: raw.exists === true,
    state: str(raw.state) ?? "ready",
    recovery: optStr(raw.recovery),
    sourceId: optStr(raw.source_id),
  };
}

export type AssetLibrary = { assets: GeneratedAsset[]; openRequests: HostJob[] };

export function interpretAssetLibrary(result: unknown): AssetLibrary {
  const r = isObj(result) ? result : {};
  const assets = (Array.isArray(r.assets) ? r.assets : [])
    .map(interpretAsset)
    .filter((a): a is GeneratedAsset => a !== null);
  const openRequests = (Array.isArray(r.open_requests) ? r.open_requests : [])
    .map(interpretJobRow)
    .filter((j): j is HostJob => j !== null);
  return { assets, openRequests };
}

/** `assets.place` placement modes (PlaceMode on the engine). */
export type PlaceMode = "append" | "broll" | "narration" | "music";

/**
 * Which place modes an asset kind supports. Mirrors the engine's
 * `RequestedKind::default_place_mode` (crates/cutright-engine/src/genright_cmd.rs):
 * sfx and music land as a music bed, voice as narration or bed, and visual or
 * transform kinds append or cover B-roll.
 */
export function placeModesFor(kind: string | null): PlaceMode[] {
  switch (kind) {
    case "voice":
    case "narration":
      return ["narration", "music"];
    case "music":
    case "sfx":
      return ["music"];
    case "image":
    case "video":
    case "broll":
    case "avatar":
    case "upscale":
    case "lipsync":
      return ["append", "broll"];
    default:
      return ["append", "broll", "narration", "music"];
  }
}
