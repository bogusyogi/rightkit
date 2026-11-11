import { invoke } from "./invoke.js";
import type { OutAttachment } from "../ipc.js";
import type { CalendarInfo, Attendee, CalEvent, EventInput, TaskListInfo, TaskItem, DriveFile, DriveListing, MeetSpace, BusySlot, FreeBusyResult, StorageQuota, DrivePermission } from "@rightkit/workspace-ui/client";
export type { CalendarInfo, Attendee, CalEvent, EventInput, TaskListInfo, TaskItem, DriveFile, DriveListing, MeetSpace, BusySlot, FreeBusyResult, StorageQuota, DrivePermission };

export function listCalendars(accountId: string): Promise<CalendarInfo[]> {
  return invoke<CalendarInfo[]>("list_calendars", { accountId });
}

export function listEvents(
  accountId: string,
  calendarId: string,
  timeMin: string,
  timeMax: string
): Promise<CalEvent[]> {
  return invoke<CalEvent[]>("list_events", { accountId, calendarId, timeMin, timeMax });
}

export function createEvent(accountId: string, calendarId: string, ev: EventInput): Promise<CalEvent> {
  return invoke<CalEvent>("create_event", { accountId, calendarId, ev });
}

export function updateEvent(
  accountId: string,
  calendarId: string,
  eventId: string,
  ev: EventInput
): Promise<void> {
  return invoke<void>("update_event", { accountId, calendarId, eventId, ev });
}

export function deleteEvent(accountId: string, calendarId: string, eventId: string): Promise<void> {
  return invoke<void>("delete_event", { accountId, calendarId, eventId });
}

export function truncateEventSeries(
  accountId: string,
  calendarId: string,
  recurringEventId: string,
  occurrenceStart: string
): Promise<void> {
  return invoke<void>("truncate_event_series", {
    accountId,
    calendarId,
    recurringEventId,
    occurrenceStart,
  });
}

export function rsvpEvent(
  accountId: string,
  calendarId: string,
  eventId: string,
  selfEmail: string,
  response: string
): Promise<void> {
  return invoke<void>("rsvp_event", { accountId, calendarId, eventId, selfEmail, response });
}

export function freeBusy(
  accountId: string,
  calendarIds: string[],
  timeMin: string,
  timeMax: string
): Promise<FreeBusyResult[]> {
  return invoke<FreeBusyResult[]>("free_busy", { accountId, calendarIds, timeMin, timeMax });
}

export function listTasks(accountId: string, showCompleted = false): Promise<TaskItem[]> {
  return invoke<TaskItem[]>("list_tasks", { accountId, showCompleted });
}

export function createTask(accountId: string, title: string, notes: string): Promise<void> {
  return invoke<void>("create_task", { accountId, title, notes });
}

export function listTaskLists(accountId: string): Promise<TaskListInfo[]> {
  return invoke<TaskListInfo[]>("list_task_lists", { accountId });
}

export function createTaskList(accountId: string, title: string): Promise<TaskListInfo> {
  return invoke<TaskListInfo>("create_task_list", { accountId, title });
}

export function renameTaskList(accountId: string, listId: string, title: string): Promise<void> {
  return invoke<void>("rename_task_list", { accountId, listId, title });
}

export function deleteTaskList(accountId: string, listId: string): Promise<void> {
  return invoke<void>("delete_task_list", { accountId, listId });
}

export function clearCompletedTasks(accountId: string, listId: string): Promise<void> {
  return invoke<void>("clear_completed_tasks", { accountId, listId });
}

export function createTaskIn(
  accountId: string,
  listId: string,
  title: string,
  notes: string,
  due: string
): Promise<void> {
  return invoke<void>("create_task_in", { accountId, listId, title, notes, due });
}

export function setTaskStatus(
  accountId: string,
  listId: string,
  taskId: string,
  completed: boolean
): Promise<void> {
  return invoke<void>("set_task_status", { accountId, listId, taskId, completed });
}

export function deleteTask(
  accountId: string,
  listId: string,
  taskId: string
): Promise<void> {
  return invoke<void>("delete_task", { accountId, listId, taskId });
}

export function listDriveFiles(
  accountId: string,
  folderId?: string,
  query?: string,
  pageToken?: string,
  shelf?: string,
  mimeFilter?: string
): Promise<DriveListing> {
  return invoke<DriveListing>("list_drive_files", {
    accountId,
    folderId: folderId ?? null,
    query: query ?? null,
    pageToken: pageToken ?? null,
    shelf: shelf ?? null,
    mimeFilter: mimeFilter ?? null,
  });
}

export function driveStorageQuota(accountId: string): Promise<StorageQuota> {
  return invoke<StorageQuota>("drive_storage_quota", { accountId });
}

export function driveAttachFile(
  accountId: string,
  fileId: string,
  mimeType: string,
  filename: string
): Promise<OutAttachment> {
  return invoke<OutAttachment>("drive_attach_file", { accountId, fileId, mimeType, filename });
}

export function driveUpload(
  accountId: string,
  filename: string,
  mimeType: string,
  data: number[],
  parentId?: string
): Promise<DriveFile> {
  return invoke<DriveFile>("drive_upload", {
    accountId,
    filename,
    mimeType,
    data,
    parentId: parentId ?? null,
  });
}

export function driveDownload(
  accountId: string,
  fileId: string,
  mimeType: string,
  filename: string
): Promise<string> {
  return invoke<string>("drive_download", { accountId, fileId, mimeType, filename });
}

export function driveCreateFolder(
  accountId: string,
  name: string,
  parentId?: string
): Promise<DriveFile> {
  return invoke<DriveFile>("drive_create_folder", {
    accountId,
    name,
    parentId: parentId ?? null,
  });
}

export function driveRename(
  accountId: string,
  fileId: string,
  newName: string
): Promise<DriveFile> {
  return invoke<DriveFile>("drive_rename", { accountId, fileId, newName });
}

export function driveMove(
  accountId: string,
  fileId: string,
  newParent: string,
  oldParent?: string
): Promise<DriveFile> {
  return invoke<DriveFile>("drive_move", {
    accountId,
    fileId,
    newParent,
    oldParent: oldParent ?? null,
  });
}

export function driveTrash(accountId: string, fileId: string): Promise<void> {
  return invoke<void>("drive_trash", { accountId, fileId });
}

export function driveShare(
  accountId: string,
  fileId: string,
  email: string,
  role: string
): Promise<string> {
  return invoke<string>("drive_share", { accountId, fileId, email, role });
}

export function driveListPermissions(accountId: string, fileId: string): Promise<DrivePermission[]> {
  return invoke<DrivePermission[]>("drive_list_permissions", { accountId, fileId });
}

export function driveRevokePermission(
  accountId: string,
  fileId: string,
  permissionId: string
): Promise<void> {
  return invoke<void>("drive_revoke_permission", { accountId, fileId, permissionId });
}

export function createMeet(accountId: string): Promise<MeetSpace> {
  return invoke<MeetSpace>("create_meet", { accountId });
}
