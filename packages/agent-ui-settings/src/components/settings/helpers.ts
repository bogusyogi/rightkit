import type { McpServerRow, McpServerType } from "@rightkit/agent-ui-core/lib/types";

export function formatIsoTime(value: string) {
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  return date.toLocaleString();
}

export function shortHash(value: string | undefined) {
  if (!value) return "missing";
  return value.length > 12 ? `${value.slice(0, 12)}...` : value;
}

export function mcpServerKey(server: Pick<McpServerRow, "scope" | "name">) {
  return `${server.scope}:${server.name}`;
}

export function describeMcpServer(server: McpServerRow) {
  if (server.config.type === "http" || server.config.type === "sse") {
    return server.config.url;
  }
  const args = server.config.args?.join(" ") ?? "";
  return `${server.config.command}${args ? ` ${args}` : ""}`;
}

export function mcpTrustLabel(preset: string | undefined): string {
  switch (preset) {
    case "local_process":
      return "local process";
    case "local_dev":
      return "local dev";
    case "remote_read_only":
      return "remote read-only";
    case "remote_full":
      return "remote full";
    case "remote_unreviewed":
      return "remote unreviewed";
    default:
      return preset || "unknown";
  }
}

export function defaultMcpTrustPreset(type: McpServerType): string {
  return type === "stdio" ? "local_process" : "local_dev";
}
