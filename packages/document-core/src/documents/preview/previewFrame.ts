// Frame-source construction for documents served from the `viewright-preview` custom protocol
// (the shell's preview store, configured in src-tauri/src/preview_protocol.rs). The protocol is
// registered by the Rust side; this file talks to the shell's register/revoke commands and takes the
// iframe `src` from the URL the host builds. Content is keyed by an unguessable token, never by
// file path.

import { PREVIEW_COMMANDS, type PreviewHandle } from "@rightkit/tauri-shell";

export type PreviewKind = "mdx" | "interactive";

export const PREVIEW_SCHEME = "viewright-preview";

/** The origin a registered token is served from: `viewright-preview://localhost/<token>` on
 *  macOS/Linux, `http://viewright-preview.localhost/<token>` on Windows/WebView2. */
function isPreviewUrl(url: string, token: string): boolean {
  return (
    url === `${PREVIEW_SCHEME}://localhost/${token}` ||
    url === `http://${PREVIEW_SCHEME}.localhost/${token}`
  );
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
