// The workspace data model and the typed client the host supplies. No transport lives here.

export interface CalendarInfo {
  id: string;
  summary: string;
  background_color: string;
  primary: boolean;
  access_role: string;
}

export interface Attendee {
  email: string;
  response_status: string;
}

export interface CalEvent {
  id: string;
  calendar_id: string;
  summary: string;
  description: string;
  location: string;
  start: string;
  end: string;
  all_day: boolean;
  html_link: string;
  meet_link: string;
  organizer: string;
  attendees: Attendee[];
  my_response: string;
  is_recurring: boolean;
  recurring_event_id: string;
}

export interface EventInput {
  summary: string;
  description?: string;
  location?: string;
  startRfc3339: string;
  endRfc3339: string;
  allDay?: boolean;
  timeZone: string;
  attendees?: string[];
  reminderMinutes?: number | null;
  addMeet?: boolean;
  recurrence?: string | null;
}

export interface TaskListInfo {
  id: string;
  title: string;
}

export interface TaskItem {
  id: string;
  title: string;
  notes: string;
  due: string;
  status: string;
  list_id: string;
  list_title: string;
}

export interface DriveFile {
  id: string;
  name: string;
  mime_type: string;
  modified_time: string;
  web_view_link: string;
  size: string;
  icon_link: string;
  thumbnail_link: string;
  is_folder: boolean;
  parents: string[];
}

export interface DriveListing {
  files: DriveFile[];
  next_page_token: string;
}

export interface MeetSpace {
  name: string;
  meeting_uri: string;
  meeting_code: string;
}

export interface BusySlot {
  start: string;
  end: string;
}

export interface FreeBusyResult {
  calendar_id: string;
  busy: BusySlot[];
}

export interface StorageQuota {
  limit: string;
  usage: string;
  usage_in_drive: string;
}

export interface DrivePermission {
  id: string;
  role: string;
  kind: string;
  email: string;
  display_name: string;
}

export interface Recipient {
  name: string;
  email: string;
  photo_url: string;
}

export interface ContactFull {
  resource_name: string;
  etag: string;
  display_name: string;
  given_name: string;
  family_name: string;
  emails: string[];
  phones: string[];
  org: string;
  photo_url: string;
}

export interface ContactGroup {
  resource_name: string;
  name: string;
  member_count: number;
}

/** The event editor's form state (the calendar view's create/edit dialog). */
export type EventForm = {
  id?: string;
  summary: string;
  start: string;
  end: string;
  allDay: boolean;
  attendees: string;
  reminderMinutes: string;
  addMeet: boolean;
  recurrence: string;
  recurEnd: "never" | "until" | "count";
  recurUntil: string;
  recurCount: string;
  recurByday: string[];
};

/** The new-task form. */
export type TaskCreate = { title: string; notes: string };

/** Duplicate contacts found by the dedupe tool: each primary with the entries that repeat it. */
export type ContactDupes = { primary: ContactFull; dups: ContactFull[] }[];

/** The Drive shelves the nav offers. */
export type DriveShelf = "my" | "recent" | "shared" | "starred" | "trash";

/**
 * Every call the workspace views make. The host implements it over its own transport (MailRight: Tauri commands
 * in `@rightkit/mail-ui/lib/ipc`); the views never name a command. `accountId` is the host's account key.
 */
export interface WorkspaceClient {
  // Calendar
  listCalendars(accountId: string): Promise<CalendarInfo[]>;
  listEvents(accountId: string, calendarId: string, timeMin: string, timeMax: string): Promise<CalEvent[]>;
  createEvent(accountId: string, calendarId: string, ev: EventInput): Promise<CalEvent>;
  updateEvent(accountId: string, calendarId: string, eventId: string, ev: EventInput): Promise<void>;
  deleteEvent(accountId: string, calendarId: string, eventId: string): Promise<void>;
  truncateEventSeries(accountId: string, calendarId: string, recurringEventId: string, occurrenceStart: string): Promise<void>;
  rsvpEvent(accountId: string, calendarId: string, eventId: string, selfEmail: string, response: string): Promise<void>;
  freeBusy(accountId: string, calendarIds: string[], timeMin: string, timeMax: string): Promise<FreeBusyResult[]>;
  // Tasks
  listTasks(accountId: string, showCompleted?: boolean): Promise<TaskItem[]>;
  listTaskLists(accountId: string): Promise<TaskListInfo[]>;
  createTaskList(accountId: string, title: string): Promise<TaskListInfo>;
  renameTaskList(accountId: string, listId: string, title: string): Promise<void>;
  deleteTaskList(accountId: string, listId: string): Promise<void>;
  clearCompletedTasks(accountId: string, listId: string): Promise<void>;
  createTaskIn(accountId: string, listId: string, title: string, notes: string, due: string): Promise<void>;
  setTaskStatus(accountId: string, listId: string, taskId: string, completed: boolean): Promise<void>;
  deleteTask(accountId: string, listId: string, taskId: string): Promise<void>;
  // Drive
  listDriveFiles(accountId: string, folderId?: string, query?: string, pageToken?: string, shelf?: string, mimeFilter?: string): Promise<DriveListing>;
  driveStorageQuota(accountId: string): Promise<StorageQuota>;
  driveUpload(accountId: string, filename: string, mimeType: string, data: number[], parentId?: string): Promise<DriveFile>;
  /** Downloads to the host's download folder and resolves to the saved path. */
  driveDownload(accountId: string, fileId: string, mimeType: string, filename: string): Promise<string>;
  driveCreateFolder(accountId: string, name: string, parentId?: string): Promise<DriveFile>;
  driveRename(accountId: string, fileId: string, newName: string): Promise<DriveFile>;
  driveMove(accountId: string, fileId: string, newParent: string, oldParent?: string): Promise<DriveFile>;
  driveTrash(accountId: string, fileId: string): Promise<void>;
  driveShare(accountId: string, fileId: string, email: string, role: string): Promise<string>;
  driveListPermissions(accountId: string, fileId: string): Promise<DrivePermission[]>;
  driveRevokePermission(accountId: string, fileId: string, permissionId: string): Promise<void>;
  // Contacts
  searchRecipients(accountId: string, query: string): Promise<Recipient[]>;
  listAllContacts(accountId: string): Promise<ContactFull[]>;
  createContact(accountId: string, name: string, emails: string[], phones: string[]): Promise<void>;
  updateContact(accountId: string, resourceName: string, etag: string, name: string, emails: string[], phones: string[]): Promise<void>;
  deleteContact(accountId: string, resourceName: string): Promise<void>;
  listContactGroups(accountId: string): Promise<ContactGroup[]>;
  // Meet
  createMeet(accountId: string): Promise<MeetSpace>;
  // Host services the views use
  /** Opens a URL in the user's browser. */
  openExternal(url: string): Promise<void>;
  /** Saves text as a download and resolves to the saved path. */
  saveTextDownload(filename: string, text: string): Promise<string>;
}
