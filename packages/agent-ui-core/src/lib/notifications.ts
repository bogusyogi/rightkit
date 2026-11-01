// FM-174 — RightKit native notifications (macOS and Windows) with graceful
// fallback to Web Notifications and no-op when unavailable.
// Also owns dock/taskbar badge count (macOS dock, Windows taskbar).

const permissionCache: boolean | null = null;

async function tauriNotify(title: string, body: string): Promise<boolean> {
  try {
    const { createShell } = await import("@rightkit/tauri-shell");
    return (await createShell().notifications.post({ title, body })).delivered;
  } catch {
    return false;
  }
}

function webNotify(title: string, body: string): boolean {
  try {
    if (typeof Notification === "undefined") return false;
    if (Notification.permission === "granted") {
      new Notification(title, { body });
      return true;
    }
    if (Notification.permission !== "denied") {
      void Notification.requestPermission().then((perm) => {
        if (perm === "granted") new Notification(title, { body });
      });
      return false;
    }
    return false;
  } catch {
    return false;
  }
}

export async function notifyOs(title: string, body: string, deepLink?: string): Promise<void> {
  // Best-effort: tauri first, then web, then silent.
  const titleWithLink = deepLink ? title : title;
  const bodyWithLink = deepLink ? `${body}` : body;
  const sent = await tauriNotify(titleWithLink, bodyWithLink);
  if (!sent) webNotify(titleWithLink, bodyWithLink);
  // Deep-link click-through is handled by badge/title click focusing the
  // window + App.tsx sessionIdFromDeeplink for coderight://session/<id>.
  // Tauri surfaces a click on the notification as window focus; the renderer
  // can also listen to `coderight-opened-urls` for protocol clicks.
  void deepLink;
}

export function setBadgeCount(count: number): void {
  const n = Math.max(0, Math.floor(count));
  try {
    // Update document title badge (visible in every environment)
    if (typeof document !== "undefined") {
      const base = document.title.replace(/^\s*\(\d+\)\s*/, "");
      document.title = n > 0 ? `(${n}) ${base}` : base;
    }
  } catch {
    // ignore
  }
  // Best-effort: update the native dock/taskbar badge when Tauri is available.
  void (async () => {
    try {
      const { getCurrentWindow } = await import("@tauri-apps/api/window");
      await getCurrentWindow().setBadgeCount(n || undefined);
    } catch {
      // no-op
    }
  })();
  // Expose for tests / fallback consumers.
  try {
    if (typeof window !== "undefined") {
      (window as unknown as Record<string, unknown>).__coderight_badge = n;
    }
  } catch {
    // ignore
  }
  void permissionCache;
}

export function buildSessionDeepLink(sessionId: string): string {
  return `coderight://session/${encodeURIComponent(sessionId)}`;
}
