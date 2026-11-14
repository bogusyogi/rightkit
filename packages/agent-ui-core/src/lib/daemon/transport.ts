import type { DaemonConfig } from "../types.js";

/**
 * The one sanctioned raw transport to the daemon, for calls the typed
 * `requestDaemonApi` cannot express: streams (SSE), abortable requests, and
 * modules whose callers rely on their own error shapes. Every other call goes
 * through `requestDaemonApi`.
 *
 * It adds the bearer token (when the request does not carry its own
 * authorization header). A host may also hand it the engine's generated
 * OpenAPI operation table with `setDeclaredDaemonRoutes()`: from then on a
 * request to a method and path the engine does not declare is reported (console
 * error, which a desktop e2e step can fail on) instead of silently drifting from
 * the contract. With no table configured (the default) no route is checked.
 */
export interface DaemonRouteOperation {
  method: string;
  path: string;
}

let declaredRoutes: ReadonlyArray<{ method: string; pattern: RegExp }> | null = null;

export function setDeclaredDaemonRoutes(operations: readonly DaemonRouteOperation[] | null): void {
  declaredRoutes = operations
    ? operations.map((operation) => ({
        method: operation.method.toUpperCase(),
        pattern: new RegExp(
          `^${operation.path
            .replace(/[.*+?^$()|[\]\\]/g, "\\$&")
            .replace(/\\\{[A-Za-z_][A-Za-z0-9_]*\\\}|\{[A-Za-z_][A-Za-z0-9_]*\}/g, "[^/]+")}$`,
        ),
      }))
    : null;
}

/** True when the route is declared, or when no table is configured. */
export function isDeclaredDaemonRoute(method: string, pathname: string): boolean {
  if (!declaredRoutes) return true;
  const upper = method.toUpperCase();
  return declaredRoutes.some((route) => route.method === upper && route.pattern.test(pathname));
}

export async function daemonFetch(
  config: DaemonConfig,
  input: string | URL,
  init: RequestInit = {},
): Promise<Response> {
  const url = new URL(input.toString());
  const method = (init.method ?? "GET").toUpperCase();
  if (!isDeclaredDaemonRoute(method, url.pathname)) {
    console.error(`daemonFetch: ${method} ${url.pathname} is not a declared daemon route`);
  }
  const headers = new Headers(init.headers);
  if (config.token && !headers.has("authorization")) {
    headers.set("authorization", `Bearer ${config.token}`);
  }
  return fetch(url, { ...init, method, headers });
}
