import { useEffect, useState } from "react";
import type { NavGroup } from "@rightkit/react-shell";
import { AppShell } from "@rightkit/react-shell/react";
import type { PersistedOnboardingState } from "@rightkit/onboarding";
import { SettingsPanel, standardSections } from "@rightkit/settings-ui";
import "@rightkit/settings-ui/settings.css";
import { invoke } from "./lib/host";
import { legalDocuments } from "./lib/legal";
import { detectPlatform } from "./lib/platform";
import { license, licensingEnabled, settings } from "./lib/services";
import { shell, shellBridge } from "./lib/shell";
import Setup, { setupComplete } from "./Setup";
import { version } from "../package.json";

// Home and Settings. A media app adds @rightkit/media-ui for its player or viewer (README step 10).
const NAV: readonly NavGroup[] = [{ items: [{ id: "home", label: "Home" }, { id: "settings", label: "Settings" }] }];

type Phase = { kind: "loading" } | { kind: "setup" | "app"; licensing: boolean } | { kind: "error"; message: string };

export default function App() {
  const [phase, setPhase] = useState<Phase>({ kind: "loading" });
  const [active, setActive] = useState("home");
  const [doctor, setDoctor] = useState<string>("");
  const platform = detectPlatform();

  useEffect(() => {
    let live = true;
    void (async () => {
      try {
        const licensing = await licensingEnabled();
        const state = (await settings.load()).values["onboarding.state"] as PersistedOnboardingState | undefined;
        if (live) setPhase({ kind: setupComplete(state, licensing) ? "app" : "setup", licensing });
      } catch (e) {
        if (live) setPhase({ kind: "error", message: e instanceof Error ? e.message : String(e) });
      }
    })();
    return () => { live = false; };
  }, []);

  if (phase.kind === "loading") return <p className="p-4" data-testid="loading">Loading…</p>;
  if (phase.kind === "error") return <p className="p-4" role="alert" data-testid="startup-error">{phase.message}</p>;
  if (phase.kind === "setup") return <Setup licensing={phase.licensing} onDone={() => setPhase({ kind: "app", licensing: phase.licensing })} />;

  const openExternal = (url: string) => shell?.openExternal(url);
  return (
    <AppShell
      title="RightApp"
      groups={NAV}
      activeId={active}
      onNavigate={setActive}
      sidebar={false}
      sidebarToggle={false}
      searchMode="none"
      bridge={shellBridge}
      platform={platform}
    >
      {active === "settings" ? (
        <SettingsPanel
          backend={settings}
          sections={standardSections({
            appearance: false,
            about: { appName: "RightApp", version, platform, openExternal },
            license: phase.licensing ? { client: license, openExternal } : false,
            legal: { documents: legalDocuments, openExternal },
          })}
        />
      ) : (
        <div className="p-4">
          <p>Platform: <span data-testid="platform">{platform}</span></p>
          <button
            type="button"
            data-testid="run-doctor"
            className="rounded bg-brand-action px-3 py-1 text-white hover:bg-brand-action-hover"
            onClick={async () => setDoctor(JSON.stringify(await invoke("engine_call", { cmd: "doctor", input: {} })))}
          >
            Run engine doctor
          </button>
          <pre data-testid="doctor">{doctor}</pre>
        </div>
      )}
    </AppShell>
  );
}
