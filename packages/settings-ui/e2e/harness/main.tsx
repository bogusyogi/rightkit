import { StrictMode, useLayoutEffect, useMemo, useState } from "react";
import { createRoot } from "react-dom/client";
import { AppShell, PageShell, Toggle, createThemeStore, type NavGroup } from "./shell.js";
import { createAutoUpdateClient } from "@rightkit/app-update";
import { createLicenseClient, type StoredLicense } from "@rightkit/license";
import { createLogsClient } from "@rightkit/logs";
import {
  SettingRow,
  SettingsGroup,
  SettingsPanel,
  createShellBackends,
  mergeSections,
  standardSections,
  type EngineShell,
  type SettingsSection,
} from "../../src/index.js";
import "../../../react-shell/shell.css";
import "../../settings.css";

const q = new URLSearchParams(location.search);

async function rpc<T>(method: string, params?: unknown): Promise<T> {
  const res = await fetch(`/rpc/${encodeURIComponent(method)}`, { method: "POST", body: JSON.stringify(params ?? {}) });
  const body = await res.json();
  if (!res.ok) throw Object.assign(new Error(body.error ?? `rpc ${method} failed`), { code: body.code });
  return body as T;
}

const shell: EngineShell = {
  engine: {
    call: rpc,
    onEvent: async (handler) => {
      const es = new EventSource("/events");
      es.onmessage = (m) => handler(JSON.parse(m.data));
      // A navigated-away document must release its stream: the browser allows 6 connections per origin, and
      // leaked streams starve the next document's requests.
      const release = () => es.close();
      window.addEventListener("pagehide", release, { once: true });
      return () => {
        window.removeEventListener("pagehide", release);
        es.close();
      };
    },
  },
};
const backends = createShellBackends(shell);

const themeStore = createThemeStore({ storageKey: "h.theme", storage: null });

const logs = createLogsClient({
  allowedFields: ["screen", "count"],
  flushIntervalMs: 0,
  invoke: (command, args) => rpc(command.replace(/^rightkit_logs_/, "rightkit.logs."), args) as never,
});
logs.info("settings_opened", { screen: "settings", count: 1, transcript: "never on disk" });

const licenseStore = {
  get: async () => JSON.parse(localStorage.getItem("h.license") ?? "null") as StoredLicense | null,
  set: async (l: StoredLicense | null) => void (l ? localStorage.setItem("h.license", JSON.stringify(l)) : localStorage.removeItem("h.license")),
};
const license = createLicenseClient({
  app: "acme",
  host: "licenses.test",
  appVersion: "1.4.2",
  storage: licenseStore,
  deviceId: () => "dev-1",
  fetchImpl: (input, init) => fetch(String(input).replace(/^https:\/\/licenses\.test/, ""), init),
});

let updateStep = 0;
const updates = createAutoUpdateClient({
  getTier: () => "update",
  isEligible: () => true,
  canInstall: () => true,
  check: async () => ({
    version: "1.5.0",
    rawJson: { tier: "update" },
    downloadAndInstall: async (onEvent) => {
      updateStep += 1;
      onEvent?.({ event: "Started", data: { contentLength: 100 } });
      onEvent?.({ event: "Progress", data: { chunkLength: 100 } });
      onEvent?.({ event: "Finished" });
    },
  }),
});

const dictation: SettingsSection = {
  id: "dictation",
  label: "Dictation",
  group: "App",
  order: 50,
  keywords: ["voice", "microphone"],
  entries: [{ id: "dictation.cleanup", label: "Clean up transcripts", keywords: ["punctuation"] }],
  render: (ctx) => (
    <SettingsGroup title="Recognition">
      <SettingRow id="dictation.cleanup" label="Clean up transcripts" description="Punctuation and casing.">
        <Toggle label="Clean up transcripts" checked={ctx.values["dictation.cleanup"] !== false} onChange={(v) => void ctx.set("dictation.cleanup", v)} />
      </SettingRow>
    </SettingsGroup>
  ),
};

const nav: NavGroup[] = [{ items: [{ id: "home", label: "Home", icon: <svg viewBox="0 0 16 16" aria-hidden="true"><circle cx="8" cy="8" r="5" fill="currentColor" /></svg> }, { id: "settings", label: "Settings", icon: <svg viewBox="0 0 16 16" aria-hidden="true"><rect x="3" y="3" width="10" height="10" rx="2" fill="currentColor" /></svg> }] }];
const noop = async () => {};

function App() {
  const [page, setPage] = useState("settings");
  useLayoutEffect(() => {
    (window as unknown as { __ready: boolean }).__ready = true;
  }, []);
  const sections = useMemo(
    () =>
      mergeSections(
        standardSections({
          appearance: { themeStore, accents: [{ id: "brand", label: "Brand", color: "#7040C4" }, { id: "teal", label: "Teal", color: "#0C8A7B" }, { id: "amber", label: "Amber", color: "#B25000" }] },
          updates: { client: updates, supportsAutoInstall: true, currentVersion: "1.4.2", onRestart: () => void (document.title = "restarting") },
          models: { models: backends.models, title: "Speech models" },
          license: { client: license, purchaseUrl: "https://example.test/buy", openExternal: noop, tierLabels: { pro: "Pro" } },
          legal: { documents: [{ title: "Terms of Service", url: "https://example.test/terms" }, { title: "Privacy Policy", url: "https://example.test/privacy" }], acceptedAt: "2026-09-01T12:00:00Z", acceptedVersion: "2026-09", openExternal: noop, onReview: noop },
          apiKeys: { secrets: backends.secrets, slots: [{ id: "openai", label: "Provider API key", description: "Used for cloud cleanup.", ref: { service: "acme", account: "provider-key" } }, { id: "relay", label: "Relay token", ref: { service: "acme", account: "relay" } }] },
          diagnostics: { logs },
          about: { appName: "Acme", version: "1.4.2", build: "8841", platform: "macOS", copyright: "© Orthic Labs", links: [{ label: "Website", url: "https://example.test" }], openExternal: noop },
        }),
        [dictation],
        { hide: q.get("hide") ? q.get("hide")!.split(",") : [] },
      ),
    [],
  );
  return (
    <AppShell groups={nav} activeId={page} onNavigate={setPage} title={page === "home" ? "Home" : "Settings"} wordmark={<>Acme<span>Right</span></>} bridge={{ window: { minimize: noop, toggleMaximize: noop, close: noop } }} platform="macos" shell={{ themeStore }}>
      {page === "settings" ? (
        <PageShell maxWidth={0} scroll={false}>
          <SettingsPanel backend={backends.settings} sections={sections} defaultActiveId={q.get("section") ?? undefined} />
        </PageShell>
      ) : (
        <PageShell title="Home">Nothing here.</PageShell>
      )}
    </AppShell>
  );
}
createRoot(document.getElementById("root")!).render(<StrictMode><App /></StrictMode>);
