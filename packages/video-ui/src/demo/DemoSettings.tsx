import { Button, Select, Toggle } from "@rightkit/react-shell/react";
import { useEffect, useState } from "react";
import { SettingRow, SettingsGroup } from "@rightkit/settings-ui";

import {
  type DemoCatalogItem,
  type DemoPrepare,
  demoBeginPerson,
  demoBundleDiscard,
  demoBundleReveal,
  demoCatalog,
  demoErrorMessage,
  demoPrepare,
  demoSetEnabled,
  phaseLabel,
} from "./bindings";
import { useDemo } from "./useDemo";
import "./demo.css";
import { hostAppName } from "../appName";

export function DemoSettings() {
  const { status, state, refresh } = useDemo();
  const [catalog, setCatalog] = useState<DemoCatalogItem[]>([]);
  const [app, setApp] = useState("");
  const [prep, setPrep] = useState<DemoPrepare | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    demoCatalog()
      .then((c) => {
        setCatalog(c);
        setApp((cur) => cur || c[0]?.id || "");
      })
      .catch(() => setCatalog([]));
  }, []);

  if (!status) {
    return (
      <SettingsGroup>
        <SettingRow id="demo-unavailable" label="Agent demos" description="Not part of this build.">
          <span />
        </SettingRow>
      </SettingsGroup>
    );
  }

  const run = async (f: () => Promise<unknown>) => {
    setBusy(true);
    setError(null);
    try {
      await f();
      await refresh();
    } catch (e) {
      setError(demoErrorMessage(e));
    } finally {
      setBusy(false);
    }
  };
  const lastBundle = state.bundle?.name;
  const active = state.phase === "armed" || state.phase === "recording" || state.phase === "stopping" || state.phase === "awaiting_consent";

  return (
    <>
      <SettingsGroup title="Agent demos">
        <SettingRow
          id="demo-enabled"
          label="Allow agent demos"
          description=`Off by default. When on, an agent you run on this computer can ask to drive one app while ${hostAppName()} records that app's window. You approve every session. Nothing leaves this computer.`
        >
          <Toggle label="Allow agent demos" checked={status.enabled} onChange={(v) => void run(() => demoSetEnabled(v))} />
        </SettingRow>
        {status.enabled && (
          <SettingRow id="demo-kill" label="Kill control" description="Stops the agent and the recording at once, from anywhere.">
            <kbd>{status.hotkey}</kbd>
          </SettingRow>
        )}
      </SettingsGroup>

      {status.enabled && (
        <SettingsGroup title="Permissions">
          <SettingRow id="demo-permissions" label="Accessibility and Screen Recording" description=`${hostAppName()} needs both to drive and record another app. It never asks by itself.`>
            <Button
              size="sm"
              variant="secondary"
              disabled={busy}
              onClick={() => void run(async () => setPrep(await demoPrepare()))}
            >
              Check
            </Button>
          </SettingRow>
          {prep && (
            <SettingRow id="demo-permissions-result" label="Result" description={`${prep.recorder}${prep.notes.length ? `. ${prep.notes.join(" ")}` : ""}`}>
              <span>
                Accessibility: {prep.accessibility ? "granted" : "missing"}. Screen Recording: {prep.screen_recording ? "granted" : "missing"}.
              </span>
            </SettingRow>
          )}
        </SettingsGroup>
      )}

      {status.enabled && (
        <SettingsGroup title="Record myself">
          <SettingRow id="demo-person" label="Record an app" description="Records one app's window with the badge and the kill control, without an agent.">
            <Select
              label="App to record"
              value={app}
              onChange={setApp}
              disabled={busy || active}
              options={catalog.map((c) => ({ value: c.id, label: c.name }))}
            />
            <Button size="sm" disabled={busy || active || !app} onClick={() => void run(() => demoBeginPerson(app))}>
              Start recording
            </Button>
          </SettingRow>
        </SettingsGroup>
      )}

      {status.enabled && (
        <SettingsGroup title="Current state">
          <SettingRow id="demo-state" label={phaseLabel(state.phase)} description={state.message ?? undefined}>
            {lastBundle ? (
              <span className="cr-demo-inline">
                <Button size="sm" variant="secondary" disabled={busy} onClick={() => void run(() => demoBundleReveal(lastBundle))}>
                  Show last recording
                </Button>
                <Button
                  size="sm"
                  variant="danger"
                  disabled={busy || active}
                  onClick={() => void run(() => demoBundleDiscard(lastBundle))}
                >
                  Delete it
                </Button>
              </span>
            ) : (
              <span />
            )}
          </SettingRow>
        </SettingsGroup>
      )}

      {error && (
        <p role="alert" className="cr-demo-error">
          {error}
        </p>
      )}
    </>
  );
}
