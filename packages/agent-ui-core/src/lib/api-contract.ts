import { DAEMON_OPENAPI_OPERATIONS } from "./generated/daemon-openapi";

export interface DaemonApiEndpoint {
  method: string;
  path: string;
  summary: string;
}

export interface DaemonApiContract {
  name: "coderight-daemon";
  version: number;
  endpoints: DaemonApiEndpoint[];
}

/** Runtime endpoint inventory projected from Rust-owned generated OpenAPI metadata. */
export const DAEMON_API_ENDPOINTS = DAEMON_OPENAPI_OPERATIONS.map((operation) => ({
  method: operation.method,
  path: operation.path.replace(/\{([A-Za-z_][A-Za-z0-9_]*)\}/g, ":$1"),
  summary: operation.summary,
})) satisfies readonly DaemonApiEndpoint[];
