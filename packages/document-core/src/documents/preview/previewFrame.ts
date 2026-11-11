// Frame-source construction for documents served from the host's `<host id>-preview` custom
// protocol (the shell's preview store). The protocol is
// registered by the Rust side; this file talks to the shell's register/revoke commands and takes the
// iframe `src` from the URL the host builds. Content is keyed by an unguessable token, never by
// file path.

import { PREVIEW_COMMANDS, type PreviewHandle } from "@rightkit/tauri-shell";
import { hostScheme } from "../../host.js";

export type PreviewKind = "mdx" | "interactive";

/** The host's preview scheme, `<host id>-preview` (`hostScheme`). */
export const previewScheme = (): string => hostScheme("preview");

/** The origin a registered token is served from: `<scheme>://localhost/<token>` on
 *  macOS/Linux, `http://<scheme>.localhost/<token>` on Windows/WebView2. */
function isPreviewUrl(url: string, token: string): boolean {
  const scheme = previewScheme();
  return url === `${scheme}://localhost/${token}` || url === `http://${scheme}.localhost/${token}`;
}

export { isTauri as inTauriWebview } from "@rightkit/platform-ui";

export async function registerPreview(kind: PreviewKind, html: string): Promise<PreviewHandle> {
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke<PreviewHandle>(PREVIEW_COMMANDS.register, { kind, html });
}

export async function revokePreview(token: string): Promise<void> {
  try {
    const { invoke } = await import("@tauri-apps/api/core");
    await invoke(PREVIEW_COMMANDS.revoke, { token });
  } catch {
    /* best-effort: the token dies with the process */
  }
}

export async function registeredPreviewSrc(
  kind: PreviewKind,
  html: string,
): Promise<{ token: string; src: string }> {
  const { token, url } = await registerPreview(kind, html);
  // The host builds the URL from its own scheme. Re-check the shape anyway: a token that is not the
  // 64-hex form, or a URL outside the preview origin, never reaches an iframe.
  if (!/^[0-9a-f]{64}$/.test(token) || !isPreviewUrl(url, token)) {
    void revokePreview(token);
    throw new Error("invalid preview registration");
  }
  return { token, src: url };
}
