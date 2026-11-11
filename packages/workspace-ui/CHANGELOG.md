# Changelog

## 0.2.0 (unpublished)

Taken on the recommended option of the shape review, pending the owner's confirmation.

- BREAKING: the views render over a host the app supplies (`WorkspaceHostProvider`, `WorkspaceHost`) and import
  nothing from `@rightkit/mail-ui`: no store, hooks, IPC, icons or account switcher of the mail app. Calls go
  through the typed `WorkspaceClient`; shared state through the host's `useState` selector over the
  `WorkspaceState` slices; errors, notices, dialogs, preferences, compose, downloads and view switches through
  host actions; the account switcher is an optional host component.
- Moved here from `@rightkit/mail-ui` (deleted there): the Drive, calendar, contacts and tasks navs, `useDrive`,
  the calendar helpers (`calendarUtils`, `WEEKDAYS`), the avatar helpers (`colorFor`, `initialOf`) and the
  workspace data model (`CalEvent`, `DriveFile`, `ContactFull`, `EventForm`, ...). mail-ui imports them from here.
- Glyphs from `lucide-react` (`Icon`), not the mail app's icon set.
- Emailing a contact opens a new message through `composeTo` without switching the host to its inbox first.
- Dependencies: `@rightkit/mail-ui` removed; `lucide-react` added. New subpath exports `./client`, `./host`,
  `./lib/avatar`, `./lib/calendarUtils`.

## 0.1.0

- First release. Calendar, contacts, drive, tasks and meeting views for workspace-style apps.
