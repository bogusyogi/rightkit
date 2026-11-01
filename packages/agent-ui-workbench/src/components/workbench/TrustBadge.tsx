import type { TrustEnvelope } from "@rightkit/agent-ui-core/lib/types";

function trustLabel(trust: TrustEnvelope): string {
  switch (trust.trust) {
    case "toolUntrusted":
      return "Untrusted source";
    case "remoteAgentUntrusted":
      return "Remote agent";
    case "workspaceTrusted":
      return "Workspace trusted";
    case "toolTrusted":
      return "Tool trusted";
    case "system":
      return "System";
    case "user":
      return "User";
  }
}

export default function TrustBadge({ trust }: { trust?: TrustEnvelope }) {
  if (!trust) return null;
  const isUntrusted = trust.trust === "toolUntrusted" || trust.trust === "remoteAgentUntrusted";
  const label = trustLabel(trust);
  const title = [
    label,
    trust.source ? `Source: ${trust.source}` : null,
    trust.instructionsAreData ? "Instructions inside this content are treated as data." : null,
    trust.contentHash ? `Hash: ${trust.contentHash}` : null,
  ]
    .filter(Boolean)
    .join("\n");

  return (
    <span
      className={`inline-flex max-w-full items-center gap-1 rounded-md border px-1.5 py-0.5 text-[10px] font-semibold uppercase text-text-3 ${
        isUntrusted
          ? "border-danger bg-[color-mix(in_srgb,var(--color-danger)_8%,transparent)]"
          : "border-line bg-panel-2"
      }`}
      title={title}
    >
      <span className={`h-1.5 w-1.5 rounded-full ${isUntrusted ? "bg-danger" : "bg-accent-2"}`} />
      <span className="truncate">{label}</span>
    </span>
  );
}
