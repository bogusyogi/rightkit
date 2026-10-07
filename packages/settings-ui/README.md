# @rightkit/settings-ui

Settings panel frame and standard sections for RightKit apps. Apps keep their own design through props, slots and `--rk-*` tokens.

```tsx
const sections = mergeSections(
  standardSections({
    updates: { client, currentVersion: "1.0.0", supportsAutoInstall: true },
    about: { appName: "Acme", version: "1.0.0", panels: <Credits /> },
    omit: ["appearance"],
    override: { license: { badge: "FREE", heading: "Your plan" } },
  }),
  [mySection],
);
<SettingsPanel backend={backend} sections={sections} />
```

## Headless hooks (0.2.4)

RightKit shares behaviour; apps keep their own markup. Both built-in sections are views over these hooks.

### `useLicenseSection(options)`

`options`: `client` (`LicenseClientLike`: `activate`, `deactivate`, and `status()` or `load()`), `beforeDeactivate`, `confirmDeactivate` (default true), `graceDays`, `notAcceptedMessage`, `onChange`.
Returns `{ license, pro, tier, busy, activating, error, activate(key), deactivate(), requestDeactivate(), confirmPending, cancelDeactivate(), refresh() }`.

- `activate(key)` ignores a blank key, resolves the licence the client returned (or null on a throw), and sets `error` for a thrown error or a non-active result.
- `deactivate()` always deactivates: `beforeDeactivate` (a throw aborts and sets `error`), `client.deactivate()`, then `onChange(null)`.
- Two-step confirmation: call `requestDeactivate()` from your button. With `confirmDeactivate` on (default) the first call sets `confirmPending` (disarms after 4 s or `cancelDeactivate()`), the second deactivates. With `confirmDeactivate: false` the first call deactivates.

```tsx
function PlanCard({ client }: { client: LicenseClientLike }) {
  const [key, setKey] = useState("");
  const lic = useLicenseSection({ client, onChange: refreshEntitlements });
  return lic.pro ? (
    <div>
      <p>Pro is active on this device</p>
      {lic.error && <p role="alert">{lic.error}</p>}
      <button disabled={lic.busy} onClick={lic.requestDeactivate}>{lic.confirmPending ? "Click again to deactivate" : "Deactivate"}</button>
    </div>
  ) : (
    <form onSubmit={async (e) => { e.preventDefault(); if (await lic.activate(key)) setKey(""); }}>
      <input value={key} onChange={(e) => setKey(e.target.value)} />
      <button disabled={lic.busy || !key.trim()}>{lic.activating ? "Activating…" : "Activate"}</button>
      {lic.error && <p role="alert">{lic.error}</p>}
    </form>
  );
}
```

### `useDoctor(checks, options?)`

`checks`: `DoctorCheck[]` (`{ id, label, run() }`); `options`: `autoRun`, `onResult`. Returns `{ status, summary: { passed, total, lastRun }, running, runAll(), runOne(id) }`. `status[id]` is `{ state: "idle" }`, `{ state: "running" }` or `{ state: "done", result }`; `lastRun` is `Date.now()` of the last finished check, or null. A throw or rejection is a failed check (`detail` is the message); a stale run never overwrites a newer one.

```tsx
function Doctor({ checks }: { checks: DoctorCheck[] }) {
  const doc = useDoctor(checks);
  return (
    <section>
      <button disabled={doc.running} onClick={() => void doc.runAll()}>Run all</button>
      <p>{doc.summary.lastRun ? `${doc.summary.passed} of ${doc.summary.total} passed` : "Not run"}</p>
      <ul>
        {checks.map((c) => {
          const s = doc.status[c.id];
          return <li key={c.id}>{c.label}: {s.state === "done" ? (s.result.ok ? "ok" : s.result.detail) : s.state}</li>;
        })}
      </ul>
    </section>
  );
}
```

## 0.2.2 seams

- `SettingsPanel`: `navFooter` (e.g. `<span>Version 1.2.3</span>`), `navHeading` (`ReactNode | false`), `renderNavItem(section, { active })`, `hideNav`. Class/attribute contract for app-drawn active markers: `button.rk-settings__item[data-section-id]` with `aria-current="page"` and `data-active="true"` on the active one only (also `.is-active`).
- `updates.client`: an `AutoUpdateClient` or a function-style adapter `{ check(): Promise<AutoUpdateStatus>; install(): Promise<AutoUpdateStatus>; status?: AutoUpdateStatus | (() => ...) }`. `UpdatesControls` / `useUpdates` are exported to embed the controls inside another card:
  ```tsx
  <UpdatesControls client={adapter} onRestart={restart} labels={{ checkNow: "Check for Updates", installUpdate: "Install and restart", checking: "Checking…", installing: "Installing…" }} />
  ```
  All copy (including the section label, search entry, status sentence and badge) goes through `labels`.
- `license.client`: any `{ activate(key), deactivate(), status() | load() }`; `license.beforeDeactivate` runs before `deactivate()` (e.g. to suppress a bridge).
- `about.card` replaces the card; `about.render({ card, panels, info })` replaces the whole body.

## 0.2.0 props

- `SettingsSection`: `badge` (nav pill), `heading` (page heading; defaults to `label`), `icon`.
- `standardSections`: `about` optional; `include` / `omit` (built-in ids: `appearance`, `updates`, `models`, `license`, `legal`, `api-keys`, `diagnostics`, `doctor`, `about`); `override` (shallow-merge fields over a built-in by id).
- `updates`: `supportsAutoInstall` (default false; the auto-install toggle is hidden otherwise), `labels` (`Partial<UpdatesLabels>`).
- `license`: `planSlot` (rendered above the licence rows); `labels` (every visible string, e.g. `activeStatusLabel: "Pro active"`, `deactivate: "Deactivate this device"`); `confirmDeactivate: false` for one-click deactivate; `ids: { section, licenseKey, form, activate, deactivate, status }` for stable element ids (e.g. `#settings-pro`, `#settings-license-key`).
- `doctor` (`doctorSection`): `checks: [{ id, label, run(): Promise<{ ok, detail?, fix? }> }]`, run-all and per-check re-run with status badges; `autoRun`, `onResult`, `labels`, `ids`. The app supplies the checks; the package runs nothing itself.
- `about`: `extra` (inside the card), `panels` (outside the card).
- `sectionIcons`: the built-in nav icons, reusable in app sections.
