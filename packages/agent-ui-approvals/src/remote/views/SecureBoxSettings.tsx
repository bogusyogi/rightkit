import { useCallback, useEffect, useState, type ReactNode } from "react";
import { Button, Toggle, Input } from "@rightkit/react-shell/react";
import { secureBoxApi, type SecureItem, type SecureList } from "../api.js";
import { SettingRow, SettingsGroup, SettingsItem } from "@rightkit/settings-ui/core";

type Published = Record<string, unknown>;
type SetFn = (key: string, value: unknown) => void;

const message = (e: unknown): string => {
  const text = typeof e === "string" ? e : e instanceof Error ? e.message : "That did not work.";
  const m = /^(?:unlock_needs_auth|pin_not_set|pin_wrong|pin_locked|pin_invalid|invalid_params|card_not_virtual|[a-z_]+): (.*)$/s.exec(text);
  return m ? m[1] : text;
};

const dollars = (cents: number | undefined): string =>
  cents === undefined ? "none" : `$${Math.floor(cents / 100)}.${String(cents % 100).padStart(2, "0")}`;

/** A dollar amount as typed ("50", "50.25") to whole cents; null when it is not a plain amount. */
const toCents = (text: string): number | null => {
  const t = text.trim().replace(/^\$/, "");
  if (!/^\d{1,7}(\.\d{1,2})?$/.test(t)) return null;
  return Math.round(Number(t) * 100);
};

/** What `remote.secure.list` adds beyond the api type (agent_secrets.rs): the effective global caps and held outcomes. */
type Caps = {
  capAutoCents?: number; capMonthCents?: number;
  unknownCents?: Record<string, number>;
};
type CardExtra = { spentCents?: number; held?: number };

const AUTO_CAP = "remoteFormFillPaymentsAutoCapCents";
const MONTH_CAP = "remoteFormFillPaymentsMonthCapCents";
const SPOT = "remoteScreenSpotTakeover";

const asLines = (v: unknown): string[] => {
  if (Array.isArray(v)) return v.map(String);
  if (typeof v === "string" && v.trim() !== "") {
    try {
      const parsed: unknown = JSON.parse(v);
      if (Array.isArray(parsed)) return parsed.map(String);
    } catch {
      return v.split("\n").map((l) => l.trim()).filter((l) => l !== "");
    }
  }
  return [];
};

function Switch({ s, set, k, label, note, disabled }: {
  s: Published; set: SetFn; k: string; label: string; note: string; disabled?: boolean;
}) {
  if (!(k in s)) return null;
  return (
    <SettingRow className="ck-set" label={label} description={note}>
      <Toggle checked={Boolean(s[k])} disabled={disabled} onChange={(v) => set(k, v)} label={label} />
    </SettingRow>
  );
}

/** A global payment cap in dollars; the setting holds whole cents. Raising it asks for the PIN (the owner-only door). */
function CapRow({ s, set, k, label, note, disabled }: {
  s: Published; set: SetFn; k: string; label: string; note: string; disabled: boolean;
}) {
  const [draft, setDraft] = useState<string | null>(null);
  const [problem, setProblem] = useState<string | null>(null);
  if (!(k in s)) return null;
  const current = typeof s[k] === "number" ? dollars(s[k] as number) : "";
  const save = () => {
    const cents = toCents(draft ?? "");
    if (cents === null || cents <= 0) { setProblem("A cap is a dollar amount above $0, such as 25 or 100.50."); return; }
    setProblem(null);
    setDraft(null);
    set(k, cents);
  };
  return (
    <SettingRow className="ck-set" label={label} description={note}>
      <div className="ck-ctl">
        <Input className="ck-input" aria-label={label} value={draft ?? current} disabled={disabled}
          autoComplete="off" spellCheck={false} onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => { if (e.key === "Enter") save(); }} />
        <Button size="sm" variant="secondary" disabled={disabled || draft === null || draft === current} onClick={save}>Save</Button>
        {problem && <span className="ck-sub" role="alert">{problem}</span>}
      </div>
    </SettingRow>
  );
}

/** One password-manager line per row: `name=chord`. Saved when the box loses focus; empty means no manager is used. */
function ManagersRow({ s, set, disabled }: { s: Published; set: SetFn; disabled: boolean }) {
  const k = "remoteFormFillLoginManagers";
  const [draft, setDraft] = useState<string | null>(null);
  if (!(k in s)) return null;
  const published = s[k];
  const list = asLines(published);
  const commit = () => {
    const next = (draft ?? "").split("\n").map((l) => l.trim()).filter((l) => l !== "");
    setDraft(null);
    if (draft === null || next.join("\n") === list.join("\n")) return;
    set(k, typeof published === "string" ? JSON.stringify(next) : next);
  };
  return (
    <SettingRow className="ck-set" label="Password managers Pulse may start for a login"
      description="One per line as name=chord, for example 1Password=cmd+shift+\ . Pulse sends that manager's own autofill shortcut to your browser and sees nothing of what it fills; the manager's own Touch ID or Windows Hello runs in the browser. Pulse never reads a manager's vault. Empty by default: no manager is used.">
      <textarea className="ck-input" aria-label="Password managers Pulse may start for a login" rows={Math.min(6, Math.max(2, list.length + 1))}
        spellCheck={false} disabled={disabled}
        value={draft ?? list.join("\n")}
        onChange={(e) => setDraft(e.target.value)}
        onBlur={commit} />
    </SettingRow>
  );
}

/** A labelled one-line field. Never used for a secret: those are typed into the system dialog. */
function Field({ label, value, onChange, placeholder, type = "text", max }: {
  label: string; value: string; onChange: (v: string) => void; placeholder?: string; type?: string; max?: number;
}) {
  return (
    <Input type={type} className="ck-input" aria-label={label} placeholder={placeholder ?? label} value={value}
      autoComplete="off" autoCorrect="off" autoCapitalize="off" spellCheck={false} maxLength={max}
      onChange={(e) => onChange(e.target.value)} />
  );
}

type Mode = null | "login" | "card" | "totp";

const held = (item: SecureItem, caps: Caps | undefined): boolean =>
  ((item as SecureItem & CardExtra).held ?? 0) > 0 || (caps?.unknownCents?.[item.id] ?? 0) > 0;

function itemLine(item: SecureItem, spent: number | undefined, caps: Caps | undefined): string {
  if (item.kind === "login") return `Login for ${item.host ?? "an unknown site"}${item.totp ? " · one-time code saved" : ""}`;
  // The engine holds every card to the global caps; this shows what it will actually use.
  const auto = item.autoCents === undefined ? undefined : Math.min(item.autoCents, caps?.capAutoCents ?? item.autoCents);
  const month = item.monthCents === undefined ? undefined : Math.min(item.monthCents, caps?.capMonthCents ?? item.monthCents);
  const parts = [`Card ending ${item.last4 ?? "????"}`, `shops: ${(item.hosts ?? []).join(", ")}`,
    `up to ${dollars(item.maxCents)} a purchase`,
    `no question at or under ${dollars(auto)}${auto !== item.autoCents ? " (held to the cap)" : ""}`,
    `${dollars(month)} a month${month !== item.monthCents ? " (held to the cap)" : ""}`];
  const used = (item as SecureItem & CardExtra).spentCents ?? spent;
  if (used !== undefined) parts.push(`${dollars(used)} used this month, ${dollars(Math.max(0, (month ?? 0) - used))} left`);
  return parts.join(" · ");
}

/**
 * General > Agent logins and cards: the three switches, the saved items (labels, sites and limits
 * only, never a value), and the forms that add a login or a virtual card. A secret is never typed
 * here: the form sends the label, sites, limits and the PIN, and the command opens the system's own
 * dialog for the username and password, or the name, number, expiry and CVC. The PIN is held only
 * until the attempt is sent, then cleared.
 */
export function SecureBoxGroup({ s, set }: { s: Published; set: SetFn }) {
  const published = ["remoteFormFillLogins", "remoteFormFillPayments", "remoteFormFillLoginManagers", AUTO_CAP, MONTH_CAP, SPOT].some((k) => k in s);
  const [data, setData] = useState<SecureList | null>(null);
  const [mode, setMode] = useState<Mode>(null);
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);
  const [status, setStatus] = useState<string | null>(null);
  const [label, setLabel] = useState("");
  const [host, setHost] = useState("");
  const [zip, setZip] = useState("");
  const [maxD, setMaxD] = useState("");
  const [autoD, setAutoD] = useState("0");
  const [monthD, setMonthD] = useState("");
  const [virtual, setVirtual] = useState(false);
  const [withTotp, setWithTotp] = useState(false);
  const [target, setTarget] = useState<SecureItem | null>(null);
  const [pin, setPin] = useState("");

  const load = useCallback(() => {
    secureBoxApi.list().then((r) => { setData(r); setProblem(null); }).catch((e: unknown) => setProblem(message(e)));
  }, []);
  useEffect(() => { if (published) load(); }, [published, load]);

  if (!published) return null;
  const toolsOn = Boolean(s.remoteTools);

  const reset = () => {
    setMode(null); setLabel(""); setHost(""); setZip(""); setMaxD(""); setAutoD("0"); setMonthD(""); setVirtual(false); setWithTotp(false); setTarget(null); setPin("");
  };
  const open = (next: Mode, item: SecureItem | null = null) => { reset(); setMode(next); setTarget(item); setProblem(null); setStatus(null); };

  const finish = (saved: { cancelled?: true } | unknown) => {
    if ((saved as { cancelled?: boolean } | null)?.cancelled) { setStatus("Nothing was saved: the dialog was closed."); return; }
    setStatus(mode === "login" ? "Login saved." : mode === "totp" ? "One-time code saved." : "Card saved.");
    reset();
    load();
  };
  const fail = (e: unknown) => setProblem(message(e));

  const saveLogin = () => {
    if (label.trim() === "" || host.trim() === "") { setProblem("Give the login a label and the site it is for."); return; }
    if (pin === "") { setProblem("Type your Pulse PIN to save."); return; }
    const typed = pin;
    setPin("");
    setBusy(true); setProblem(null); setStatus(null);
    secureBoxApi.saveLogin(label.trim(), host.trim(), typed, withTotp).then(finish).catch(fail).finally(() => setBusy(false));
  };
  const saveTotp = () => {
    if (!target) return;
    if (pin === "") { setProblem("Type your Pulse PIN to save."); return; }
    const typed = pin;
    setPin("");
    setBusy(true); setProblem(null); setStatus(null);
    secureBoxApi.setTotp(target.id, target.host ?? "", typed).then(finish).catch(fail).finally(() => setBusy(false));
  };
  const saveCard = () => {
    const max = toCents(maxD), auto = toCents(autoD), month = toCents(monthD);
    const hosts = host.split(/[\s,]+/).map((h) => h.trim()).filter((h) => h !== "");
    if (label.trim() === "" || hosts.length === 0) { setProblem("Give the card a label and the shops it may be used on."); return; }
    if (!virtual) { setProblem("Only a virtual or tokenized card can be saved. Make one at your card issuer, limited to the shops you use."); return; }
    if (max === null || max <= 0 || auto === null || month === null || month <= 0) { setProblem("Limits are dollar amounts: the most for one purchase and for a month above $0, and the no-question amount."); return; }
    if (auto > max) { setProblem("The no-question amount cannot be above the most for one purchase."); return; }
    if (!/^\d{5}(-?\d{4})?$/.test(zip.trim())) { setProblem("The billing ZIP is a US ZIP code."); return; }
    if (pin === "") { setProblem("Type your Pulse PIN to save."); return; }
    const typed = pin;
    setPin("");
    setBusy(true); setProblem(null); setStatus(null);
    secureBoxApi.saveCard({ label: label.trim(), hosts, zip: zip.trim(), maxCents: max, autoCents: auto, monthCents: month }, typed)
      .then(finish).catch(fail).finally(() => setBusy(false));
  };
  const remove = (item: SecureItem) => {
    setBusy(true); setProblem(null); setStatus(null);
    secureBoxApi.remove(item.id)
      .then(() => { setStatus(`Removed ${item.label}.`); load(); })
      .catch(fail)
      .finally(() => setBusy(false));
  };

  const items = data?.items ?? [];
  const caps = data as (SecureList & Caps) | null;
  const capsLine = caps?.capAutoCents !== undefined && caps?.capMonthCents !== undefined
    ? `Global caps: ${dollars(caps.capAutoCents)} a purchase without asking and ${dollars(caps.capMonthCents)} a month across all cards. A higher limit here is held to them.`
    : "Pulse also holds every card to two global caps, set above.";
  const auto = toCents(autoD), monthCents = toCents(monthD);
  const heldNotes = [
    caps?.capAutoCents !== undefined && auto !== null && auto > caps.capAutoCents ? `The no-question amount will be held to ${dollars(caps.capAutoCents)}.` : null,
    caps?.capMonthCents !== undefined && monthCents !== null && monthCents > caps.capMonthCents ? `The month limit will be held to ${dollars(caps.capMonthCents)}.` : null,
  ].filter((n): n is string => n !== null).join(" ");
  const form = (title: string, fields: ReactNode, submit: () => void, cta: string, hint: string) => (
    <div className="ck-set ck-stack" role="group" aria-label={title}>
      <span className="ck-sub">{hint}</span>
      <div className="ck-ctl">{fields}</div>
      <div className="ck-ctl">
        <Field label="Pulse PIN" type="password" value={pin} onChange={setPin} max={64} />
        <Button size="sm" variant="primary" disabled={busy} onClick={submit}>{cta}</Button>
        <Button size="sm" variant="ghost" disabled={busy} onClick={reset}>Cancel</Button>
      </div>
    </div>
  );

  return (
    <SettingsGroup hideWhenEmpty headingLevel={2} className="ck-sgroup" rowsClassName="ck-card" title="Agent logins and cards">
      <Switch s={s} set={set} k="remoteFormFillLogins" label="Let agents sign in with logins saved in Pulse" disabled={!toolsOn}
        note="Lets an agent ask Pulse to type one saved field (username or password) into the page open in your browser. Pulse reads the page's real address, requires it to match the site you saved, and asks you with Touch ID, Windows Hello or your PIN each time. The agent never sees the value. Needs Remote tools on. Off by default." />
      <ManagersRow s={s} set={set} disabled={!toolsOn || !Boolean(s.remoteFormFillLogins)} />
      <Switch s={s} set={set} k="remoteFormFillPayments" label="Let agents pay with a virtual card saved in Pulse" disabled={!toolsOn}
        note="Lets an agent ask Pulse to type one field of a saved virtual or tokenized US card into a verified checkout page, inside the limits you set on the card. Up to the no-question amount (never above the cap below) it pays without asking; above it you confirm each purchase with Touch ID or your PIN, every time (Windows uses the PIN: Windows Hello is not used for approvals yet). Pressing the pay button still asks. The amount and the shop's name come from the agent and Pulse cannot check them, so the caps are the real limit. The agent never sees the card. Needs Remote tools on. Off by default." />
      <CapRow s={s} set={set} k={AUTO_CAP} disabled={!toolsOn} label="Largest purchase Pulse approves without asking"
        note="Across all cards, whatever a card's own no-question amount says. Above it, Touch ID or your PIN confirms each purchase. Default $25. Raising it asks for your PIN." />
      <CapRow s={s} set={set} k={MONTH_CAP} disabled={!toolsOn} label="Most agents may spend in a calendar month"
        note="Across all cards, whatever a card's own monthly limit says. Default $100. Raising it asks for your PIN." />
      <Switch s={s} set={set} k={SPOT} label="Let an agent hand you one part of the screen" disabled={!toolsOn}
        note="Lets an agent ask you to do one step yourself (a login, a payment confirmation, a CAPTCHA). Pulse dims everything except that region, shows what the agent is about to fill and where, and has a Stop button. The agent never sees the region. Needs Remote tools on. Off by default." />
      {items.length === 0 && (
        <SettingRow className="ck-set" label="Saved logins and cards"
          description="Nothing saved yet. Only labels, sites and limits are ever listed here, never a username, password or card number." />
      )}
      {items.map((item) => (
        <SettingsItem key={item.id} text={`${item.label} ${itemLine(item, undefined, caps ?? undefined)}`} className="ck-set">
          <div className="ck-text">
            <span>{item.label}</span>
            <span className="ck-sub">{itemLine(item, item.kind === "card" ? data?.spentCents?.[item.id] : undefined, caps ?? undefined)}</span>
            {item.kind === "card" && held(item, caps ?? undefined) && (
              <span className="ck-sub" role="status">A purchase with this card ended without Pulse knowing whether it went through. Its amount stays counted, and Pulse asks you before it pays on that site again. Check the card's own statement.</span>
            )}
          </div>
          <div className="ck-ctl">
            {item.kind === "login" && (
              <Button size="sm" variant="ghost" disabled={busy} aria-label={`${item.totp ? "Replace" : "Add"} one-time code for ${item.label}`}
                onClick={() => open("totp", item)}>{item.totp ? "Replace one-time code…" : "Add one-time code…"}</Button>
            )}
            <Button size="sm" variant="ghost" disabled={busy} aria-label={`Remove ${item.kind} ${item.label}`} onClick={() => remove(item)}>Remove</Button>
          </div>
        </SettingsItem>
      ))}
      <SettingRow className="ck-set" label="Add to the secure box"
        description="Saved in your system credential store with your Pulse PIN. You type the label, site and limits here; the secret fields are typed into a system dialog that opens next, never into this page. Set a PIN under Remote access first if you have none.">
        <div className="ck-ctl">
          <Button size="sm" variant="secondary" disabled={busy} onClick={() => open("login")}>Add login…</Button>
          <Button size="sm" variant="secondary" disabled={busy} onClick={() => open("card")}>Add virtual card…</Button>
        </div>
      </SettingRow>
      {mode === "login" && form("Add a login",
        <>
          <Field label="Label" placeholder="Label, for example Work email" value={label} onChange={setLabel} max={64} />
          <Field label="Site" placeholder="Site, for example example.com" value={host} onChange={setHost} />
          <Toggle checked={withTotp} onChange={setWithTotp} label="Also save a one-time code (authenticator) secret for this login" />
        </>,
        saveLogin, "Continue to username and password…",
        "Next, a system dialog asks for the username and the password, then a second one for the one-time code secret if you chose that. The login can be used only on this exact site.")}
      {mode === "totp" && target && form(`${target.totp ? "Replace" : "Add"} a one-time code for ${target.label}`,
        <></>,
        saveTotp, "Continue to one-time code…",
        "Next, a system dialog asks for the one-time code secret (the setup key or the otpauth:// address). The saved username and password are kept.")}
      {mode === "card" && form("Add a virtual card",
        <>
          <Field label="Label" placeholder="Label, for example Groceries card" value={label} onChange={setLabel} max={64} />
          <Field label="Shops" placeholder="Shops, separated by commas" value={host} onChange={setHost} />
          <Field label="Billing ZIP" value={zip} onChange={setZip} max={10} />
          <Field label="Most for one purchase, dollars" value={maxD} onChange={setMaxD} />
          <Field label="No question at or under, dollars" value={autoD} onChange={setAutoD} />
          <Field label="Most for a month, dollars" value={monthD} onChange={setMonthD} />
          <Toggle checked={virtual} onChange={setVirtual} label="This is a virtual or tokenized card from my issuer, limited to these shops" />
        </>,
        saveCard, "Continue to card details…",
        `US virtual or tokenized cards only. ${capsLine} Next, two system dialogs ask for the name and number, then the expiry and the security code.${heldNotes ? ` ${heldNotes}` : ""}`)}
      {mode === "card" && !virtual && (
        <span className="ck-sub" role="alert">Pulse refuses a card that is not virtual or tokenized: a real card number is never saved. Make a virtual card at your issuer first.</span>
      )}
      {problem && <span className="ck-sub" role="alert">{problem}</span>}
      {status && <span className="ck-sub" role="status">{status}</span>}
    </SettingsGroup>
  );
}
