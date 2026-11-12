# Changelog

## 0.2.1

- Fix: Settings · Agents read the pre-0.3.0 agent wire only (`a.accounts.join`, `r.account_id`); rightkit-agent-gateway 0.3.0 renamed them `scopes` and `scope`, so the agent list threw and the activity log showed no account with a 0.3.0 host. New `approvals/lib/agentWire` (root export): `agentScopes` (`scopes ?? accounts`) and `auditScope` (`scope ?? account_id`); `SettingsAgentsSection` and `SettingsAgentsAudit` read through them, so both gateway versions work. Needs `@rightkit/mail-ui` 0.2.1 (its rows carry both names).

## 0.2.0 (unpublished)

Follows the `@rightkit/system-ui` 0.2.0 redraw (recommended option of the shape review, pending the owner's
confirmation).

- BREAKING: the Monitor and Overview screens call system-ui's host-supplied clients, so the app must call
  `configureSystemHost` from `@rightkit/system-ui` before rendering them.
- Moved here from `@rightkit/system-ui`, whose only users these screens are: the device-link, remote-approval,
  remote-unlock, browser-login, secure-box and notch-setting clients and types (`src/remote/api.ts`, still over
  Pulse's hub commands), the launcher and nearby settings sections, `settingsKit`, `qr`, `chatKind`,
  `membrane.css` and `settings.css`. `SHOW_ONBOARDING` is defined by the Settings screen that fires it.
- Requires `@rightkit/mail-ui` 0.2.0 and `@rightkit/system-ui` 0.2.0.
- Not changed, recorded for its own redraw: `src/remote` is Pulse's hub (Settings, Overview, Monitor, Devices,
  pairing, remote approvals, browser logins, secure box) and still names Pulse and its commands.

## 0.1.0

- First release. Agent approval queue and cards, agent settings, remote approvals, owner PIN and secure-box settings views.
