/** The chat kinds the host app knows, in the order grouped lists show them. Any other kind is listed after these. */
export const CHAT_KIND_ORDER: readonly string[] = ["claude", "codex", "muse"];

/** The name a chat kind shows as: Claude, Codex, Muse, or any other kind with its first letter capitalised. */
export function chatKindName(kind: string): string {
  switch (kind) {
    case "claude":
      return "Claude";
    case "codex":
      return "Codex";
    case "muse":
      return "Muse";
    default:
      return kind.charAt(0).toUpperCase() + kind.slice(1);
  }
}
