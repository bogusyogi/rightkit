# @rightkit/workspace-ui

Calendar, contacts, drive, tasks and meeting views, and their navs, for workspace-style apps. The views hold no
IPC, no store and no product name: they render over a typed client and state the host supplies.

| Item | Value |
|---|---|
| Where it runs | App webview UI (React) |
| Version | 0.2.0 |
| License | MIT OR Apache-2.0 |

## Entry points

| Import | Contents |
|---|---|
| `@rightkit/workspace-ui` | Views (`CalendarView`, `ContactsView`, `DriveView`, `TasksView`, `MeetView`, the calendar and Drive dialogs), navs (`CalendarNav`, `ContactsNav`, `DriveNav`, `TasksNav`), `useDrive`, the host contract, the data model, `Icon`, vCard and avatar helpers, calendar helpers. |
| `@rightkit/workspace-ui/client` | The data model (`CalEvent`, `DriveFile`, `ContactFull`, ...) and `WorkspaceClient`. |
| `@rightkit/workspace-ui/host` | `WorkspaceHost`, `WorkspaceHostProvider`, `useWorkspaceHost`, `useWorkspaceState`, `WorkspaceState`. |
| `@rightkit/workspace-ui/lib/avatar` | `colorFor`, `initialOf` (coloured-initial avatars). |
| `@rightkit/workspace-ui/lib/calendarUtils` | Day and week arithmetic, `rangeFor`, `eventDay`, `WEEKDAYS`. |

## Host contract

Render the views and navs inside one `<WorkspaceHostProvider host={host}>`. `host` is a `WorkspaceHost`:

| Field | What the host supplies |
|---|---|
| `client` | `WorkspaceClient`: every calendar, tasks, Drive, contacts and Meet call, plus `openExternal` and `saveTextDownload`, over the host's own transport. |
| `useState` | A selector hook over the host's state holding the `WorkspaceState` slices (`calendars`, `events`, `drivePath`, `taskLists`, ... with `set*` setters, and `activeAccount`). A zustand store with those fields satisfies it directly, so the host's other UI can share them. |
| `reportError`, `notify`, `dialog` | The host's error banner, toast, and confirm/prompt dialog. |
| `savePreference` | Persists `shownCals`, `calView`, `driveViewMode`. |
| `composeTo`, `linkIntoCompose` | Open a new message; put a Drive link into the open message (or a new one). |
| `saveDownload` | Saves (and may scan) a Drive download. |
| `openView` | Switches to another workspace view (Meet's "schedule" opens the calendar). |
| `AccountSwitcher` | Optional; shown in each view's header. |

MailRight's host is `mailWorkspaceHost` in `@rightkit/mail-ui`.

Glyphs come from Lucide (`lucide-react`); the package imports no app's icon set. Hosts bundle the package with
Vite; Tailwind hosts add `node_modules/@rightkit/workspace-ui/dist/**/*.js` to their content globs.

## Testing

```sh
pnpm --dir packages/workspace-ui typecheck
```
