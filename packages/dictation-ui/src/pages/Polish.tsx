// Polish page — grouped form grammar in one 920px column: a "Local Cleanup"
// group (free, on-device), an "AI Polish" group (Pro, own provider key), and
// the smart-polish trigger cards as their own equal-height row. The Provider
// key row expands IN PLACE (disclosure under the row) to the key panel — no
// modal. Only the toggle-on path enables polish after a save.

import { useEffect, useRef, useState } from "react";
import ErrorBanner from "../components/ErrorBanner.js";
import {
  CompactSettingRow,
  CompactSettingsGroup,
} from "../components/settings/CompactSettingsGroup.js";
import { useSettings } from "../hooks/useSettings.js";
import { dictationClient } from "../host.js";
import { useDictationEvent } from "../hooks/useDictationEvent.js";
import { AI_POLISH_PRIVACY_NOTICE } from "../lib/privacyDisclosure.js";
import { cx, ui } from "../lib/ui.js";
import { getPlatform } from "@rightkit/platform-ui";
import { PageShell, Toggle } from "@rightkit/react-shell/react";
import ApiKeysPanel from "./polish/ApiKeysPanel.js";
import { SmartPolishCard } from "./polish/PolishCards.js";

export default function PolishPage() {
  const { loaded, error, setError, patch, get } = useSettings();
  const [hasAiKey, setHasAiKey] = useState(false);
  const [keyError, setKeyError] = useState("");
  const [clearKeyErrorSignal, setClearKeyErrorSignal] = useState(0);
  // pro=null until the license loads. AI polish is Pro and needs a saved
  // provider key. Local cleanup stays free.
  const [pro, setPro] = useState<boolean | null>(() => dictationClient().entitlement());
  // Provider-key disclosure: flipping AI polish on with no saved key opens it
  // under the Provider key row; the row's button toggles it later to add, view,
  // or clear keys.
  const [keyPanelOpen, setKeyPanelOpen] = useState(false);
  // Only the toggle-on path turns polish on after a save; opening the panel to
  // view or manage keys must never change whether transcripts are sent.
  const enablePolishOnSaveRef = useRef(false);
  const keyPanelRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    if (!keyPanelOpen) return;
    // Bring the panel into view and put the caret in the key field (when no key
    // is saved yet) so the toggle-on path lands somewhere useful.
    const id = window.requestAnimationFrame(() => {
      const panel = keyPanelRef.current;
      if (!panel) return;
      panel.scrollIntoView({ block: "nearest" });
      panel.querySelector<HTMLElement>("input")?.focus();
    });
    return () => window.cancelAnimationFrame(id);
  }, [keyPanelOpen]);

  useEffect(() => {
    void dictationClient()
      .aiKeyStatus()
      .then((status) => setHasAiKey(status.groq_saved || status.openrouter_saved))
      .catch(() => undefined);
  }, []);

  useDictationEvent("entitlement", setPro);

  const cleanup = get<"clean" | "aggressive">("transcript_cleanup", "aggressive");
  const aiEnabled = get("ai_polish_enabled", false);

  function onAiToggle(next: boolean) {
    if (next && !hasAiKey) {
      // A saved provider key is mandatory to proceed — even on Apple-capable
      // Macs, where it's the required cloud fallback.
      enablePolishOnSaveRef.current = true;
      setKeyPanelOpen(true);
      return;
    }
    void patch("ai_polish_enabled", next);
  }

  return (
    <PageShell maxWidth={1320} className="dictation-page dictation-page--polish">
      <div className="grid gap-[var(--space-5)]">
        {error || keyError ? (
          <ErrorBanner
            message={error || keyError}
            onDismiss={() => {
              setError("");
              setKeyError("");
              setClearKeyErrorSignal((signal) => signal + 1);
            }}
          />
        ) : null}

        {loaded && (
          <>
            <CompactSettingsGroup
              title="Local cleanup"
              footer="Basic cleanup (casing, punctuation, spacing) always runs, fully on-device."
            >
              <CompactSettingRow
                label="Advanced cleanup"
                sublabel="Removes filler and repeated starts. Off keeps wording closer to raw speech."
                settingId="polish.advanced-cleanup"
              >
                <Toggle
                  checked={cleanup === "aggressive"}
                  onChange={(next) =>
                    void patch("transcript_cleanup", next ? "aggressive" : "clean")
                  }
                  label="Advanced cleanup"
                />
              </CompactSettingRow>
            </CompactSettingsGroup>

            <CompactSettingsGroup
              title="AI polish"
              aside={
                pro === false ? (
                  <span className="inline-flex items-center gap-[5px] rounded-full bg-[color-mix(in_srgb,var(--accent),transparent_86%)] px-[9px] py-[3px] text-[length:var(--size-xs)] font-semibold text-[var(--accent)]">
                    Pro
                  </span>
                ) : undefined
              }
              footer={AI_POLISH_PRIVACY_NOTICE}
            >
              {pro === null ? (
                // UX-13: license still loading. The group can't show the Pro
                // upsell (we don't know yet) NOR the active controls (we
                // don't know yet) — flash a quiet placeholder so the layout
                // doesn't claim "AI polish is on" mid-load.
                <div className="flex items-center gap-[var(--space-3)] px-[var(--space-4)] py-[var(--space-4)] text-[length:var(--size-sm)] text-[var(--muted)]">
                  <span
                    aria-hidden="true"
                    className="h-3 w-3 animate-pulse rounded-full bg-[var(--accent)]"
                  />
                  Checking your license…
                </div>
              ) : pro === false ? (
                // Not Pro: no toggle at all — AI polish simply isn't available yet.
                <CompactSettingRow
                  label="AI polish is a Pro feature"
                  sublabel="Clearer wording, prompt shaping, and summaries with your own provider key."
                  settingId="polish.polish-with-ai"
                >
                  <button
                    type="button"
                    className={cx(ui.button, ui.buttonSecondary, ui.buttonCompact)}
                    onClick={() => {
                      window.location.hash = "#about";
                    }}
                  >
                    Enter license
                  </button>
                  <button
                    type="button"
                    className={cx(ui.button, ui.buttonCompact)}
                    onClick={() => void dictationClient().openPricing()}
                  >
                    Get Pro
                  </button>
                </CompactSettingRow>
              ) : (
                <>
                  <CompactSettingRow
                    label="Polish with AI"
                    sublabel="Sends text to your provider only while on. Audio stays on this device."
                    settingId="polish.polish-with-ai"
                  >
                    <Toggle checked={aiEnabled} onChange={onAiToggle} label="Polish with AI" />
                  </CompactSettingRow>
                  <CompactSettingRow
                    label="Provider key"
                    sublabel="Groq is the supported provider."
                    settingId="polish.provider-key"
                  >
                    <span
                      className={cx(
                        "mr-[var(--space-2)] text-[length:var(--size-xs)]",
                        hasAiKey ? "text-[var(--muted)]" : "text-[var(--paper)]",
                      )}
                    >
                      {hasAiKey ? (getPlatform() === "macos" ? "Saved in Keychain" : "Saved securely on this device") : "Required"}
                    </span>
                    <button
                      type="button"
                      className={cx(
                        ui.button,
                        ui.buttonSecondary,
                        ui.buttonCompact,
                        "min-w-[96px]",
                      )}
                      aria-expanded={keyPanelOpen}
                      aria-controls="polish-provider-key-panel"
                      onClick={() => {
                        // Managing keys never opts into AI polish; only the
                        // toggle-on path (onAiToggle) does.
                        enablePolishOnSaveRef.current = false;
                        setKeyPanelOpen((open) => !open);
                      }}
                    >
                      {keyPanelOpen ? "Done" : hasAiKey ? "Manage key" : "Add key"}
                    </button>
                  </CompactSettingRow>
                  {keyPanelOpen ? (
                    <div
                      id="polish-provider-key-panel"
                      ref={keyPanelRef}
                      className="px-[var(--space-4)] py-[var(--space-4)]"
                    >
                      <ApiKeysPanel
                        expanded
                        inline
                        onLiveChange={setHasAiKey}
                        onSaved={() => {
                          if (enablePolishOnSaveRef.current) {
                            enablePolishOnSaveRef.current = false;
                            void patch("ai_polish_enabled", true);
                            setKeyPanelOpen(false);
                          }
                        }}
                        onKeyErrorChange={setKeyError}
                        clearKeyErrorSignal={clearKeyErrorSignal}
                      />
                    </div>
                  ) : null}
                </>
              )}
            </CompactSettingsGroup>

            {pro && aiEnabled && hasAiKey && (
              // Smart-polish trigger words are separate features, not a
              // choice between the two — say one at the end of a dictation.
              <section className="min-w-0">
                <div className="mx-[4px] mb-[8px]">
                  <h2 className={ui.sectionLabel}>Smart polish</h2>
                </div>
                <div className="grid gap-[var(--space-3)] sm:grid-cols-2">
                  <SmartPolishCard
                    trigger="prompt"
                    title="Prompt engineering"
                    description="Turns a rough dictated ask into a clearer prompt for chats, coding apps, and image tools."
                  />
                  <SmartPolishCard
                    trigger="summarize"
                    title="Summarize"
                    description="Condenses a dictation into a brief: key points, decisions, and action items."
                  />
                </div>
                <p className="m-0 mx-[14px] mt-[7px] text-[length:var(--size-xs)] leading-[1.45] text-[var(--muted)]">
                  Say a trigger word at the end of a dictation. Each works on its own.
                </p>
              </section>
            )}
          </>
        )}
      </div>
    </PageShell>
  );
}
