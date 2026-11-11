// The host contract of the workspace views (calendar, contacts, drive, tasks, meet and their navs).
//
// The package holds no IPC, no store and no product name. The host supplies, once, above the views:
//  - `client`: every data call (`WorkspaceClient`), over the host's own transport;
//  - `useState`: a selector hook over the host's state that holds the `WorkspaceState` slices (a zustand store
//    with these fields and `set*` setters satisfies it directly), so the views and the host's navs share state;
//  - the host actions the views trigger: errors, notices, dialogs, saved preferences, compose, downloads, view
//    switches, and an optional account switcher.
import { createContext, useContext, type ComponentType, type ReactNode } from "react";
import type {
  CalEvent,
  CalendarInfo,
  ContactDupes,
  ContactGroup,
  DriveFile,
  DriveShelf,
  EventForm,
  StorageQuota,
  TaskCreate,
  TaskItem,
  TaskListInfo,
  WorkspaceClient,
} from "./client.js";

/** The workspace slices of the host's state. */
export interface WorkspaceFields {
  // Calendar
  calendars: CalendarInfo[];
  selectedCal: string;
  shownCals: string[];
  calView: "month" | "week" | "day";
  calCursor: number;
  events: CalEvent[];
  openEvent: CalEvent | null;
  eventForm: EventForm | null;
  freeBusyMsg: string;
  calendarBusy: boolean;
  /** A pending "new event" seed (Meet's "schedule for later" fills it, the calendar opens the form with it). */
  eventSeed: { summary: string; withMeet: boolean } | null;
  // Tasks
  gtasks: TaskItem[];
  taskLists: TaskListInfo[];
  taskListId: string;
  taskDue: string;
  showCompletedTasks: boolean;
  listMgrOpen: boolean;
  taskFormOpen: boolean;
  taskCreate: TaskCreate;
  tasksBusy: boolean;
  // Drive
  driveFiles: DriveFile[];
  drivePath: { id: string; name: string }[];
  driveQuery: string;
  driveToken: string;
  driveShelf: DriveShelf;
  driveType: string;
  driveViewMode: "grid" | "list";
  driveQuota: StorageQuota | null;
  drivePreview: DriveFile | null;
  driveBusy: boolean;
  // Contacts
  contactGroups: ContactGroup[];
  contactsQuery: string;
  contactsBusy: boolean;
  contactDupes: ContactDupes | null;
}

/** A setter that takes a value or an updater, like React's `setState`. */
export type FieldSetter<T> = (v: T | ((prev: T) => T)) => void;

/** `setX` for every field `x`. */
export type WorkspaceSetters = {
  [K in keyof WorkspaceFields as `set${Capitalize<K & string>}`]: FieldSetter<WorkspaceFields[K]>;
};

/** What `useState` selects from. `activeAccount` is the host's current account key (read only). */
export type WorkspaceState = WorkspaceFields & WorkspaceSetters & { activeAccount: string | null };

/** The preferences the views persist through the host. */
export interface WorkspacePreferences {
  shownCals: string[];
  calView: "month" | "week" | "day";
  driveViewMode: "grid" | "list";
}

export type WorkspaceDialog = { kind: "confirm"; message: string } | { kind: "prompt"; message: string; initial?: string };

/** The views a workspace view can ask the host to show. */
export type WorkspaceView = "calendar" | "contacts" | "drive" | "tasks" | "meet";

export interface WorkspaceHost {
  client: WorkspaceClient;
  /** Selector hook over the host's state. */
  useState: <T>(selector: (s: WorkspaceState) => T) => T;
  /** Shows an error to the user. */
  reportError(message: string): void;
  /** Shows a short notice (the host's toast). */
  notify(message: string): void;
  /** Confirm or prompt. Resolves to "ok" (confirm), the entered text (prompt), or null when cancelled. */
  dialog(cfg: WorkspaceDialog): Promise<string | null>;
  /** Persists a view preference. */
  savePreference<K extends keyof WorkspacePreferences>(key: K, value: WorkspacePreferences[K]): void;
  /** Opens a new message to `to` with `subject` and an HTML `body`. */
  composeTo(to: string, subject: string, body: string): void;
  /**
   * Puts an HTML link into the message being written, or into a new message when none is open. Resolves to which
   * one happened, so the view can say so.
   */
  linkIntoCompose(html: string): "inserted" | "new-message";
  /** Runs a download the host saves (and may scan) under `label`. */
  saveDownload(label: string, download: () => Promise<string>): void;
  /** Switches the host to another workspace view. */
  openView(view: WorkspaceView): void;
  /** The host's account switcher, shown in each view's header. */
  AccountSwitcher?: ComponentType;
}

const HostContext = createContext<WorkspaceHost | null>(null);

/** Supplies the host to every workspace view and nav below it. */
export function WorkspaceHostProvider({ host, children }: { host: WorkspaceHost; children?: ReactNode }) {
  return <HostContext.Provider value={host}>{children}</HostContext.Provider>;
}

export function useWorkspaceHost(): WorkspaceHost {
  const host = useContext(HostContext);
  if (!host) throw new Error("@rightkit/workspace-ui: render the views inside <WorkspaceHostProvider host={...}>");
  return host;
}

/** Selects from the host's workspace state. */
export function useWorkspaceState<T>(selector: (s: WorkspaceState) => T): T {
  return useWorkspaceHost().useState(selector);
}

export function useWorkspaceClient(): WorkspaceClient {
  return useWorkspaceHost().client;
}

/** The host's account switcher, or nothing. */
export function HostAccountSwitcher() {
  const Switcher = useWorkspaceHost().AccountSwitcher;
  return Switcher ? <Switcher /> : null;
}
