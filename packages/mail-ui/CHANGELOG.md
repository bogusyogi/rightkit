# Changelog

## 0.2.0 (unpublished)

Taken on the recommended option of the shape review (the `@rightkit/workspace-ui` redraw), pending the owner's
confirmation.

- BREAKING: the workspace navs (`CalendarNav`, `ContactsNav`, `DriveNav`, `TasksNav`), `useDrive`, `calendarUtils`
  and `WEEKDAYS` moved to `@rightkit/workspace-ui` and are deleted here, with the `./hooks/useDrive` and
  `./lib/calendarUtils` exports. The workspace types (`CalEvent`, `DriveFile`, `ContactFull`, `EventForm`, ...),
  `colorFor` and `initialOf` are workspace-ui's; mail-ui re-exports them from there.
- New `mailWorkspaceHost` and `mailWorkspaceClient`: MailRight's `WorkspaceHost` (its Tauri commands, its store,
  toast, dialog, preferences, compose and download flows). `Sidebar` renders the workspace navs over it.
- Depends on `@rightkit/workspace-ui` (the dependency ran the other way before).

## 0.1.0

- First release. Mail client UI: inbox, reader, compose editor, recipients, attachments, account switcher and the mail store.
