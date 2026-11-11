import { useEffect, useState } from "react";
import { dictationClient, type AiKeyCheck, type AiKeyProvider, type AiKeysUpdate, type AiKeyStatus } from "../../host.js";
import { cx, ui } from "../../lib/ui.js";
import { getPlatform } from "@rightkit/platform-ui";
import { AI_POLISH_PRIVACY_NOTICE } from "../../lib/privacyDisclosure.js";
import { hostAppName } from "../../host.js";

type L3KeyStatus = AiKeyStatus;
type L3KeysUpdate = AiKeysUpdate;
type KeyCheckResult = AiKeyCheck;
type ProviderKey = AiKeyProvider;

const blankStatus: L3KeyStatus = {
  groq_saved: false,
  openrouter_saved: false,
  nvidia_saved: false,
};

// `expanded` = the first-run setup state (no key yet): render open with setup
// guidance. Otherwise render collapsed under
// "Advanced · API keys" — the keys just live there once one is saved.
// `inline` = the disclosure under the Polish "Provider key" row: just the key
// boxes (the row and group footer already carry the heading and privacy text).
export default function ApiKeysPanel({
  expanded = false,
  inline = false,
  onLiveChange,
  onSaved,
  onKeyErrorChange,
  clearKeyErrorSignal,
}: {
  expanded?: boolean;
  inline?: boolean;
  /** Reports whether any key is saved — including the mount-time fetch. */
  onLiveChange: (live: boolean) => void;
  /** Fires only after the user explicitly saves a key. */
  onSaved?: () => void;
  onKeyErrorChange: (error: string) => void;
  clearKeyErrorSignal: number;
}) {
  const [keys, setKeys] = useState(blankStatus);
  const [groqKey, setGroqKey] = useState("");
  const [keyError, setKeyError] = useState("");
  const [keyNotice, setKeyNotice] = useState("");
  const [saving, setSaving] = useState<ProviderKey | null>(null);

  function updateKeyError(next: string) {
    setKeyError(next);
    onKeyErrorChange(next);
  }

  function updateKeys(next: L3KeyStatus) {
    setKeys(next);
    onLiveChange(next.groq_saved || next.openrouter_saved);
  }

  // biome-ignore lint/correctness/useExhaustiveDependencies: mount-only status fetch; helpers are stable closures
  useEffect(() => {
    void dictationClient()
      .aiKeyStatus()
      .then(updateKeys)
      .catch((e) => updateKeyError(String(e)));
  }, []);

  useEffect(() => {
    if (!keyNotice) return;
    const timeout = window.setTimeout(() => setKeyNotice(""), 1800);
    return () => window.clearTimeout(timeout);
  }, [keyNotice]);

  // biome-ignore lint/correctness/useExhaustiveDependencies: fires only when the parent bumps the clear signal
  useEffect(() => {
    if (!keyError) return;
    updateKeyError("");
  }, [clearKeyErrorSignal]);

  const privacyNotice = (
    <p className={cx(ui.fieldNote, "m-0 max-w-[720px]")}>{AI_POLISH_PRIVACY_NOTICE}</p>
  );

  async function saveKey(which: ProviderKey, value: string) {
    if (!value) return;
    updateKeyError("");
    setKeyNotice("");
    setSaving(which);
    try {
      const check = await dictationClient().validateAiKey(which, value);
      if (check.kind === "Rejected") {
        updateKeyError(`Key rejected by ${providerLabel(which)}.`);
        return;
      }
      const update: L3KeysUpdate = {};
      if (which === "groq") update.groq_key = value;
      else if (which === "openrouter") update.openrouter_key = value;
      const status = await dictationClient().setAiKeys(update);
      updateKeys(status);
      onSaved?.();
      if (which === "groq") setGroqKey("");
      setKeyNotice(
        check.kind === "Unverified"
          ? `${providerLabel(which)} key saved — couldn't verify (offline?).`
          : `${providerLabel(which)} key saved.`,
      );
    } catch (e) {
      updateKeyError(String(e));
    } finally {
      setSaving(null);
    }
  }

  async function clearKey(which: ProviderKey) {
    updateKeyError("");
    setKeyNotice("");
    const update: L3KeysUpdate = {
      clear_groq: which === "groq",
      clear_openrouter: which === "openrouter",
    };
    try {
      const status = await dictationClient().setAiKeys(update);
      updateKeys(status);
      setKeyNotice(`${providerLabel(which)} key cleared.`);
    } catch (e) {
      updateKeyError(String(e));
    }
  }

  const boxes = (
    <div className="grid gap-[var(--space-4)]">
      <KeyBox
        title="Groq"
        badge="Primary"
        saved={keys.groq_saved}
        value={groqKey}
        onChange={setGroqKey}
        onSave={() => void saveKey("groq", groqKey)}
        onClear={() => void clearKey("groq")}
        saving={saving === "groq"}
        placeholder="gsk_..."
        signupLink="https://console.groq.com/login"
        keyLink="https://console.groq.com/keys"
        note={`Groq gives individuals a free tier. ${hostAppName()} sends polish requests to a fast open model on your own Groq quota. Sign up, create a key, and paste it back here.`}
      />
      <div
        aria-live="polite"
        className={cx(
          ui.mutedXs,
          "transition-opacity duration-200 ease-[var(--ease-out-strong)]",
          keyNotice ? "opacity-100" : "opacity-0",
        )}
      >
        {keyNotice || " "}
      </div>
    </div>
  );

  if (inline) return boxes;

  if (expanded) {
    return (
      <div>
        <h4 className="m-0 text-[length:var(--size-lg)] font-semibold text-[var(--paper)]">
          Provider key
        </h4>
        <p className={cx(ui.fieldNote, "mb-[var(--space-3)] mt-[2px] max-w-[720px]")}>
          AI polish needs at least one provider key.
        </p>
        {privacyNotice}
        {boxes}
      </div>
    );
  }

  return (
    <div>
      {privacyNotice}
      <details className="group mt-[var(--space-3)] border-t border-[color-mix(in_srgb,var(--line),transparent_22%)] pt-[var(--space-3)]">
        <summary className="mb-[var(--space-3)] cursor-pointer select-none list-none text-[length:var(--size-sm)] font-medium text-[var(--muted)] hover:text-[var(--paper)] [&::-webkit-details-marker]:hidden">
          <span className="mr-[6px] inline-block transition-transform duration-150 group-open:rotate-90">
            ▶
          </span>
          Advanced · API keys
        </summary>
        <div className="hr-details-fade">{boxes}</div>
      </details>
    </div>
  );
}

function providerLabel(which: ProviderKey) {
  if (which === "groq") return "Groq";
  return "OpenRouter";
}

function KeyBox({
  title,
  badge,
  saved,
  value,
  onChange,
  onSave,
  onClear,
  saving,
  placeholder,
  signupLink,
  keyLink,
  note,
}: {
  title: string;
  badge: string;
  saved: boolean;
  value: string;
  onChange: (value: string) => void;
  onSave: () => void;
  onClear: () => void;
  saving: boolean;
  placeholder: string;
  signupLink: string;
  keyLink: string;
  note: string;
}) {
  return (
    <div>
      <div className="mb-[var(--space-2)] flex items-center justify-between gap-[var(--space-3)]">
        <div className="flex items-center gap-[var(--space-2)]">
          <span className="font-medium text-[var(--paper)]">{title}</span>
          <span className={ui.statusPill}>{badge}</span>
          <span className={ui.mutedXs}>{saved ? "· key saved" : ""}</span>
        </div>
        <div className="flex flex-wrap justify-end gap-[var(--space-2)] text-[length:var(--size-xs)]">
          <a
            href={signupLink}
            target="_blank"
            rel="noreferrer"
            className="text-[var(--accent-cool)] underline"
          >
            Sign up
          </a>
          <a
            href={keyLink}
            target="_blank"
            rel="noreferrer"
            className="text-[var(--accent-cool)] underline"
          >
            Create key
          </a>
        </div>
      </div>
      <div className="flex flex-wrap gap-[var(--space-2)]">
        {saved ? (
          <div className="min-h-[38px] min-w-[240px] flex-1 rounded-[var(--radius-sm)] border border-[color-mix(in_srgb,var(--line),transparent_12%)] bg-[var(--surface-1)] px-[var(--space-3)] py-[var(--space-2)] text-[length:var(--size-sm)] text-[var(--muted)]">
            {getPlatform() === "macos" ? "Saved in Keychain" : "Saved securely on this device"}
          </div>
        ) : (
          <input
            className={cx(ui.input, "min-w-[240px] flex-1")}
            type="password"
            value={value}
            onChange={(e) => onChange(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") onSave();
            }}
            placeholder={placeholder}
            aria-label={`${title} API key`}
            autoComplete="off"
          />
        )}
        {saved ? (
          <button type="button" className={cx(ui.button, ui.buttonGhost)} onClick={onClear}>
            Clear
          </button>
        ) : (
          <button type="button" className={ui.button} disabled={saving || !value} onClick={onSave}>
            {saving ? "Saving…" : "Save"}
          </button>
        )}
      </div>
      <p className={cx(ui.fieldNote, "max-w-[720px]")}>{note}</p>
    </div>
  );
}
