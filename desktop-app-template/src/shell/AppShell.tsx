// PLACEHOLDER for `@rightkit/app-shell` (UPCOMING, not published). Provides the frameless window
// chrome (title bar + traffic lights/Windows buttons), drag region and content slot. When the
// package is published: add it to package.json at an exact version, delete this file, and import
// { AppShell } from "@rightkit/app-shell" in App.tsx (same props).
import type { ReactNode } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { isTauri } from "../lib/host";

const act = (f: (w: ReturnType<typeof getCurrentWindow>) => Promise<unknown>) => () => {
  if (isTauri()) void f(getCurrentWindow()).catch(() => undefined);
};

export function AppShell({ title, children }: { title: string; children: ReactNode }) {
  return (
    <div className="flex h-screen flex-col" data-testid="app-shell">
      <header data-tauri-drag-region className="flex h-9 items-center justify-between bg-brand-soft px-3 text-brand-ink">
        <span data-tauri-drag-region>{title}</span>
        <span className="flex gap-2">
          <button type="button" aria-label="Minimize" onClick={act((w) => w.minimize())}>–</button>
          <button type="button" aria-label="Maximize" onClick={act((w) => w.toggleMaximize())}>□</button>
          <button type="button" aria-label="Close" onClick={act((w) => w.close())}>×</button>
        </span>
      </header>
      <main className="flex-1 overflow-auto p-4">{children}</main>
    </div>
  );
}
