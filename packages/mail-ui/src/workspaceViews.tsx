// The @rightkit/workspace-ui views, ready to render in MailRight: each one already sits inside
// <WorkspaceHostProvider host={mailWorkspaceHost}>, so a host can mount (or lazy-load) them on their own.
// workspace-ui's bare views throw at render without that provider.
import type { ReactNode } from "react";
import {
  CalendarView as WorkspaceCalendarView,
  ContactsView as WorkspaceContactsView,
  DriveView as WorkspaceDriveView,
  MeetView as WorkspaceMeetView,
  TasksView as WorkspaceTasksView,
  WorkspaceHostProvider,
} from "@rightkit/workspace-ui";
import { mailWorkspaceHost } from "./workspaceHost.js";

/** Supplies MailRight's workspace host (`mailWorkspaceHost`) to every workspace view and nav below it. */
export function MailWorkspaceProvider({ children }: { children?: ReactNode }) {
  return <WorkspaceHostProvider host={mailWorkspaceHost}>{children}</WorkspaceHostProvider>;
}

/** workspace-ui's `CalendarView` inside `MailWorkspaceProvider`. */
export function CalendarView() {
  return (
    <MailWorkspaceProvider>
      <WorkspaceCalendarView />
    </MailWorkspaceProvider>
  );
}

/** workspace-ui's `TasksView` inside `MailWorkspaceProvider`. */
export function TasksView() {
  return (
    <MailWorkspaceProvider>
      <WorkspaceTasksView />
    </MailWorkspaceProvider>
  );
}

/** workspace-ui's `DriveView` inside `MailWorkspaceProvider`. */
export function DriveView() {
  return (
    <MailWorkspaceProvider>
      <WorkspaceDriveView />
    </MailWorkspaceProvider>
  );
}

/** workspace-ui's `ContactsView` inside `MailWorkspaceProvider`. */
export function ContactsView() {
  return (
    <MailWorkspaceProvider>
      <WorkspaceContactsView />
    </MailWorkspaceProvider>
  );
}

/** workspace-ui's `MeetView` inside `MailWorkspaceProvider`. */
export function MeetView() {
  return (
    <MailWorkspaceProvider>
      <WorkspaceMeetView />
    </MailWorkspaceProvider>
  );
}
