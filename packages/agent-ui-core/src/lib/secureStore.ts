export async function openExternal(url: string): Promise<void> {
  // In the Tauri webview, window.open of an external http(s) URL is routed to the
  // user's default browser; in a plain browser it opens a new tab.
  window.open(url, "_blank", "noopener");
}
