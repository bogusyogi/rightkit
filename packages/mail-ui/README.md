# @rightkit/mail-ui

Mail client UI: inbox, reader, compose editor, recipients, attachments, account switcher and the mail store.

| Item | Value |
|---|---|
| Where it runs | App webview UI (React) |
| Version | 0.2.1 |
| License | MIT OR Apache-2.0 |

## Entry points

| Import | Contents |
|---|---|
| `@rightkit/mail-ui` | Every module whose exported names do not collide (generated barrel). |
| `@rightkit/mail-ui/<path>` | Any module under `src/` by path without extension, e.g. `@rightkit/mail-ui/appName`. |
| `@rightkit/mail-ui/<path>.css` | Stylesheets shipped next to their components. |

## Peer dependencies

`react`

## Host data

Legal and version data stay in the host app. Alias `@rightkit-host` to the host project root
(Vite `resolve.alias`); the package imports `@rightkit-host/legal/legal-manifest.json`,
`@rightkit-host/src/generated/legal-contract.json`, `@rightkit-host/package.json` and
`@rightkit-host/legal/*.md?raw` where it needs them.

## Product name

User-facing copy reads the product name from `hostAppName()` (default `"App"`). Call
`setHostAppName("Your App")` (exported from the root and from `appName`) once at startup, before
rendering.

## Workspace views

The calendar, contacts, Drive, tasks and Meet views and their navs are `@rightkit/workspace-ui`'s, and they throw
at render without a `WorkspaceHostProvider`. This package supplies MailRight's host for them and ready wrappers:

- `CalendarView`, `TasksView`, `DriveView`, `ContactsView`, `MeetView` (root export): workspace-ui's views already
  inside the provider over `mailWorkspaceHost`. Mount or lazy-load these, not the bare views from workspace-ui:
  `lazy(() => import("@rightkit/mail-ui").then((m) => ({ default: m.CalendarView })))`.
- `MailWorkspaceProvider`: the provider over `mailWorkspaceHost`, for anything else from workspace-ui (navs,
  dialogs, `useDrive`). `Sidebar` renders the navs inside it.
- `mailWorkspaceHost` and `mailWorkspaceClient`: the host itself.

## Host contract

Hosts bundle this package with Vite (the build keeps extensionless relative imports and asset imports
such as `?raw`). Tailwind hosts add `node_modules/@rightkit/mail-ui/dist/**/*.js` to their content globs.

## Testing

```sh
pnpm --dir packages/mail-ui test
```

Modules left out of the root barrel because an export name collides with an earlier module
(import them by subpath): `lib/ipc/agentApprovals.ts`, `lib/ipc/agents.ts`, `lib/ipc/attachments.ts`, `lib/ipc/compose.ts`, `lib/ipc/legal.ts`, `lib/ipc/mail.ts`, `lib/ipc/people.ts`, `lib/ipc/pro.ts`, `lib/ipc/qaGate.ts`, `lib/ipc/qaMockMail.ts`, `lib/ipc/secure.ts`, `lib/ipc/semantic.ts`, `lib/ipc/system.ts`, `lib/ipc/workspace.ts`, `store/types.ts`
