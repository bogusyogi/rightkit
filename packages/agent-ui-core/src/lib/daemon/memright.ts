import type { DaemonConfig } from "../types";
import { requestDaemonApi } from "../generated/daemon-openapi";

export interface MemrightListRow {
  id: string;
  tier: string;
  chars: number;
  access: number;
  inject: number;
}

export interface MemrightEntry {
  id: string;
  content: string;
  access_count: number;
}

export interface MemrightRecallHit {
  id: string;
  skel: string;
  scope?: string;
  score: number;
  cos?: number;
}

export interface MemrightMetrics {
  recalls?: number;
  tokens_saved?: number;
  [key: string]: unknown;
}

export interface MemrightHealth {
  ok: boolean;
  embedder_dim: number;
}

export async function memrightMetrics(config: DaemonConfig): Promise<MemrightMetrics> {
  return requestDaemonApi<MemrightMetrics>(config, "getMemrightMetrics", {});
}

export async function memrightHealth(config: DaemonConfig): Promise<MemrightHealth> {
  return requestDaemonApi<MemrightHealth>(config, "getMemrightHealth", {});
}

export async function memrightList(
  config: DaemonConfig,
  scope?: string,
): Promise<MemrightListRow[]> {
  return requestDaemonApi<MemrightListRow[]>(config, "postMemrightList", {
    body: { scope: scope?.trim() || undefined },
  });
}

export async function memrightGet(config: DaemonConfig, id: string): Promise<MemrightEntry> {
  return requestDaemonApi<MemrightEntry>(config, "postMemrightGet", {
    body: { id },
  });
}

export async function memrightPut(
  config: DaemonConfig,
  entry: { name: string; content: string; scope?: string; tier?: string },
): Promise<{ put: string }> {
  return requestDaemonApi<{ put: string }>(config, "postMemrightPut", {
    body: entry,
  });
}

export async function memrightDelete(
  config: DaemonConfig,
  id: string,
): Promise<{ deleted: boolean }> {
  return requestDaemonApi<{ deleted: boolean }>(config, "postMemrightDelete", {
    body: { id },
  });
}

export async function memrightRecall(
  config: DaemonConfig,
  query: string,
  k = 12,
  scope?: string,
): Promise<MemrightRecallHit[]> {
  return requestDaemonApi<MemrightRecallHit[]>(config, "postMemrightRecall", {
    body: { query, k, scope: scope?.trim() || undefined },
  });
}

export interface MemrightRecallEvent {
  ts: string;
  scope?: string | null;
  query_chars: number;
  hits: number;
  injected_chars: number;
}

export interface MemrightTransformEvent {
  ts: string;
  verb: string;
  before: number;
  after: number;
}

export interface MemrightActivity {
  recalls: MemrightRecallEvent[];
  transforms: MemrightTransformEvent[];
}

export async function memrightActivity(config: DaemonConfig): Promise<MemrightActivity> {
  return requestDaemonApi<MemrightActivity>(config, "getMemrightActivity", {});
}
