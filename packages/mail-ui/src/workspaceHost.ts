// MailRight's host for the @rightkit/workspace-ui views and navs: its Tauri commands as the WorkspaceClient, its
// store as the workspace state, and its toast, dialog, preferences, compose and download flows as the host actions.
import type { WorkspaceClient, WorkspaceHost } from "@rightkit/workspace-ui";
import { AccountSwitcher } from "./components/AccountSwitcher.js";
import { showDialog } from "./hooks/dialog.js";
import {
  clearCompletedTasks,
  createContact,
  createEvent,
  createMeet,
  createTaskIn,
  createTaskList,
  deleteContact,
  deleteEvent,
  deleteTask,
  deleteTaskList,
  driveCreateFolder,
  driveDownload,
  driveListPermissions,
  driveMove,
  driveRename,
  driveRevokePermission,
  driveShare,
  driveStorageQuota,
  driveTrash,
  driveUpload,
  freeBusy,
  listAllContacts,
  listCalendars,
  listContactGroups,
  listDriveFiles,
  listEvents,
  listTaskLists,
  listTasks,
  openExternal,
  renameTaskList,
  rsvpEvent,
  saveTextDownload,
  searchRecipients,
  setTaskStatus,
  truncateEventSeries,
  updateContact,
  updateEvent,
} from "./lib/ipc.js";
import { setPref } from "./lib/preferences.js";
import { composeEditorRef, useAppStore } from "./store/index.js";

export const mailWorkspaceClient: WorkspaceClient = {
  listCalendars,
  listEvents,
  createEvent,
  updateEvent,
  deleteEvent,
  truncateEventSeries,
  rsvpEvent,
  freeBusy,
  listTasks,
  listTaskLists,
  createTaskList,
  renameTaskList,
  deleteTaskList,
  clearCompletedTasks,
  createTaskIn,
  setTaskStatus,
  deleteTask,
  listDriveFiles,
  driveStorageQuota,
  driveUpload,
  driveDownload,
  driveCreateFolder,
  driveRename,
  driveMove,
  driveTrash,
  driveShare,
  driveListPermissions,
  driveRevokePermission,
  searchRecipients,
  listAllContacts,
  createContact,
  updateContact,
  deleteContact,
  listContactGroups,
  createMeet,
  openExternal,
  saveTextDownload,
};

const store = () => useAppStore.getState();

export const mailWorkspaceHost: WorkspaceHost = {
  client: mailWorkspaceClient,
  useState: useAppStore,
  reportError: (message) => store().setErr(message),
  notify: (message) => store().showUndo(message, [], ""),
  dialog: (cfg) => showDialog(cfg),
  // The three workspace preferences are mail-ui preferences of the same names and shapes.
  savePreference: (key, value) => setPref(key, value as never),
  composeTo: (to, subject, body) => store().openComposeTo(to, subject, body),
  linkIntoCompose: (html) => {
    if (store().compose) {
      composeEditorRef.current?.insertHTML(`${html}&nbsp;`);
      return "inserted";
    }
    store().openComposeTo("", "", `${html}<br><br>`);
    return "new-message";
  },
  saveDownload: (label, download) => store().saveAndScan(label, download),
  openView: (view) => store().setView(view),
  AccountSwitcher,
};
