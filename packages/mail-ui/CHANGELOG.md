# Changelog

## 0.2.1

- Fix: the agent wire. rightkit-agent-gateway 0.3.0 renamed an agent's `accounts` to `scopes` and an audit row's `account_id` to `scope`; `lib/ipc/agents` read only the old names. `AgentRow` gains `scopes` and `AgentAuditRow` gains `scope`; `agentsList`, `agentPair`, `agentSetProfile` and `agentAuditList` normalize every row (`normalizeAgentRow`: `scopes ?? accounts`; `normalizeAgentAuditRow`: `scope ?? account_id`) and set both names, so code reading either works with hosts on both gateway versions. `agentPair` still sends `accounts`.
- Fix: the workspace views were unusable on their own. `@rightkit/workspace-ui` 0.2.0's views throw at render without a `WorkspaceHostProvider`, and MailRight lazy-loads all five without one. New root exports: `MailWorkspaceProvider` (the provider over `mailWorkspaceHost`) and `CalendarView`, `TasksView`, `DriveView`, `ContactsView`, `MeetView`, workspace-ui's views already inside it; MailRight lazy-loads these from `@rightkit/mail-ui` instead of the bare views. `Sidebar` renders the navs inside `MailWorkspaceProvider`. Needs `@rightkit/workspace-ui` 0.2.1.

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
