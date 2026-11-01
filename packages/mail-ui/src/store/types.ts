import type { ImageRule } from "../lib/imageTrust.js";
import type { Folder } from "../lib/mail.js";
import type {
  DbHealth,
  AppLockStatus,
  Account,
  Thread,
  ThreadMessage,
  ThreadRef,
  UnreadCounts,
  LabelRow,
  SendAsInfo,
  OutAttachment,
  SendReq,
  ContactCard,
  TrustReport,
  Vacation,
  FilterInfo,
  ArchiveProgress,
  CalEvent,
  TaskItem,
  DriveFile,
  StorageQuota,
  CalendarInfo,
  TaskListInfo,
  Recipient,
  ContactGroup,
  ContactFull,
  AttachmentMeta,
  OutboxRow,
} from "../lib/ipc.js";

export type View =
  | "inbox"
  | "settings"
  | "calendar"
  | "tasks"
  | "drive"
  | "contacts"
  | "meet";

export const APP_OF: Record<View, string> = {
  inbox: "mail",
  calendar: "calendar",
  drive: "drive",
  contacts: "contacts",
  tasks: "tasks",
  meet: "meet",
  settings: "settings",
};

export type BulkAction =
  | "archive"
  | "unarchive"
  | "trash"
  | "untrash"
  | "spam"
  | "unspam"
  | "read"
  | "unread"
  | "star"
  | "unstar"
  | "mute";

export const BULK_INVERSE: Partial<Record<BulkAction, string>> = {
  archive: "unarchive",
  trash: "untrash",
  untrash: "trash",
  spam: "unspam",
  unspam: "spam",
  star: "unstar",
  unstar: "star",
};

export const BULK_LABEL: Partial<Record<BulkAction, string>> = {
  archive: "Archived",
  trash: "Moved to Trash",
  untrash: "Restored",
  spam: "Reported spam",
  unspam: "Moved to Inbox",
};

export const UNDO_TOAST_MS = 6000;

export type OpenThread = {
  account_id: string;
  thread_id: string;
  subject: string;
};
export type UndoState = {
  msg: string;
  refs: ThreadRef[];
  action: string;
  at: number;
};
export type ComposeState = {
  accountId: string;
  from: string;
  to: string;
  cc: string;
  bcc: string;
  subject: string;
  body: string;
  threadId: string | null;
  inReplyTo: string | null;
  showCc: boolean;
  nonce: number;
  attachments: OutAttachment[];
  gmailDraftId: string | null;
  bodyHtml?: string;
};
export type PendingSend = { req: SendReq; draftId: string | null };
export type Presend = {
  req: SendReq;
  draftId: string | null;
  archiveRef: { account_id: string; thread_id: string } | null;
  warnings: string[];
};
export type SenderCard = {
  email: string;
  card: ContactCard | null;
  loading: boolean;
};
export type FilterBuilder = {
  from: string;
  to: string;
  subject: string;
  query: string;
  hasAttachment: boolean;
  skipInbox: boolean;
  markRead: boolean;
  star: boolean;
  trash: boolean;
  labelId: string;
};
export type TaskCreate = { title: string; notes: string };
export type CtxMenu = {
  x: number;
  y: number;
  kind: "thread" | "label";
  t?: Thread;
  labelId?: string;
};
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
export type RecipientSug = {
  field: "to" | "cc" | "bcc";
  items: Recipient[];
  groups: ContactGroup[];
};
export type ContactDupes = { primary: ContactFull; dups: ContactFull[] }[];
export type SavedSearch = { name: string; query: string };

type Set<T> = (v: T | ((prev: T) => T)) => void;

type SetterName<K extends string> = K extends "imagePolicy"
  ? "setImagePolicyState"
  : K extends "signature"
    ? "setSignatureState"
    : K extends "blockTrackers"
      ? "setBlockTrackersState"
      : `set${Capitalize<K>}`;

type FieldSetters<T extends object> = {
  [K in keyof T as K extends string ? SetterName<K> : never]: Set<T[K]>;
};

export interface AppFields {
  view: View;
  health: DbHealth | null;
  appLock: AppLockStatus | null;
  accounts: Account[];
  threads: Thread[];
  activeAccount: string | null;
  folder: Folder;
  switcherOpen: boolean;
  clientIdSet: boolean;
  clientIdInput: string;
  clientSecretSet: boolean;
  clientSecretInput: string;
  busy: string | null;
  err: string | null;
  themeChoice: string;
  fontChoice: string;
  settingsTab: string;
  soundOn: boolean;
  openThread: OpenThread | null;
  threadMsgs: ThreadMessage[];
  query: string;
  results: Thread[];
  searching: boolean;
  /** Meaning search switched on in the search box (shown only when ready). */
  meaning: boolean;
  /** Semantic search is ready, so the Meaning toggle may show. */
  meaningReady: boolean;
  /** Short note under the search box when meaning results fell back to keywords. */
  meaningNote: string | null;
  searchFocused: boolean;
  recentSearches: string[];
  savedSearches: SavedSearch[];
  counts: UnreadCounts;
  draftCount: number;
  showImages: boolean;
  reloadKey: number;
  selected: globalThis.Set<string>;
  destructiveOn: boolean;
  undo: UndoState | null;
  openLabels: string[];
  openStarred: boolean;
  moreOpenIdx: number | null;
  findOpen: boolean;
  helpOpen: boolean;
  labels: LabelRow[];
  activeLabel: string | null;
  sortMode: "newest" | "oldest";
  unreadOnly: boolean;
  loadingOlder: boolean;
  noMoreOlder: boolean;
  threadsLoading: boolean;
  hiddenLabels: globalThis.Set<string>;
  category: string | null;
  imagePolicy: boolean;
  blockTrackers: boolean;
  senderImageTrust: Record<string, ImageRule>;
  domainImageTrust: Record<string, ImageRule>;
  translateProvider: string;
  translated: Record<string, { text: string; source: string }>;
  translateKeyInput: string;
  aiMenuOpen: boolean;
  aiBusy: boolean;
  aiSummary: string | null;
  aiKeyInput: string;
  aiBaseUrl: string;
  aiModel: string;
  trustReport: TrustReport | null;
  compose: ComposeState | null;
  aliases: SendAsInfo[];
  sending: boolean;
  pendingSend: PendingSend | null;
  presend: Presend | null;
  scheduleOpen: boolean;
  scheduleAt: string;
  rawView: string | null;
  senderCard: SenderCard | null;
  movePickerOpen: boolean;
  moveFilter: string;
  snippetOpen: boolean;
  sigAlias: string;
  snipName: string;
  snipBody: string;
  aliasSigs: Record<string, string>;
  accountSigs: Record<string, string>;
  snippets: { name: string; body: string }[];
  acctEdit: Record<string, { name: string; color: string }>;
  autostartOn: boolean;
  notifyOn: boolean;
  notifyAccounts: Record<string, boolean>;
  defaultAlias: string;
  undoDelay: number;
  online: boolean;
  outbox: OutboxRow[];
  fb: FilterBuilder;
  expandAll: boolean;
  readingPane: boolean;
  multiInbox: boolean;
  keymap: Record<string, string>;
  msgAttach: Record<string, AttachmentMeta[]>;
  inlineMap: Record<string, Record<string, string>>;
  remoteImgMap: Record<string, Record<string, string>>;
  /** Per-message count of remote images short-circuited by the tracker blocklist. */
  remoteImgBlockedCount: Record<string, number>;
  expandedMsgs: globalThis.Set<number>;
  detailMsgs: globalThis.Set<number>;
  customSnoozeOpen: boolean;
  customSnoozeAt: string;
  signature: string;
  vacation: Vacation | null;
  vacationSaving: boolean;
  rotatingKey: boolean;
  archiveProg: ArchiveProgress | null;
  showPro: boolean;
  forwarding: string[];
  filters: FilterInfo[];
  labelPickerOpen: boolean;
  labelColors: Record<string, string>;
  taskCreate: TaskCreate;
  taskFormOpen: boolean;
  ctx: CtxMenu | null;
  events: CalEvent[];
  gtasks: TaskItem[];
  driveFiles: DriveFile[];
  tasksBusy: boolean;
  calendarBusy: boolean;
  driveBusy: boolean;
  calendars: CalendarInfo[];
  selectedCal: string;
  shownCals: string[];
  openEvent: CalEvent | null;
  calView: "month" | "week" | "day";
  calCursor: number;
  freeBusyMsg: string;
  eventForm: EventForm | null;
  drivePath: { id: string; name: string }[];
  driveQuery: string;
  driveToken: string;
  driveShelf: "my" | "recent" | "shared" | "starred" | "trash";
  driveType: string;
  driveViewMode: "grid" | "list";
  driveQuota: StorageQuota | null;
  drivePreview: DriveFile | null;
  taskLists: TaskListInfo[];
  taskListId: string;
  taskDue: string;
  showCompletedTasks: boolean;
  listMgrOpen: boolean;
  recipientSug: RecipientSug | null;
  contactGroups: ContactGroup[];
  contactsQuery: string;
  contactsBusy: boolean;
  contactDupes: ContactDupes | null;
  focusIdx: number;
  density: "comfortable" | "compact";
  collapsedLabels: globalThis.Set<string>;
  eventSeed: { summary: string; withMeet: boolean } | null;
}

export interface AppActions {
  showUndo: (msg: string, refs: ThreadRef[], action: string) => void;
  clearUndo: () => void;
  sigFor: (from: string, accountId?: string) => string;
  loadAliases: (accountId?: string) => void;
  openCompose: () => void;
  openComposeTo: (to: string, subject: string, body: string) => void;
  openReply: (m: ThreadMessage, all: boolean) => void;
  openForward: (m: ThreadMessage) => void;
  refresh: () => Promise<void>;
  /** Sync-complete refresh: accounts/counts/health only, no static settings. */
  refreshAfterSync: () => Promise<void>;
  /** Re-read settings a sync cannot change (client id/secret, image policy, trackers, destructive flag). */
  refreshStaticSettings: () => Promise<void>;
  loadThreads: () => Promise<void>;
  loadOlderMessages: () => Promise<void>;
  openThreadView: (t: Thread) => Promise<void>;
  saveAndScan: (label: string, download: () => Promise<string>) => void;
  toggleStar: () => void;
  toggleStarRow: (t: Thread) => void;
  toggleSelect: (t: Thread) => void;
  toggleSelectAll: () => void;
  runBulk: (action: BulkAction, refs: ThreadRef[]) => Promise<void>;
  emptyFolderFlow: (label: "TRASH" | "SPAM", count: number) => Promise<void>;
  deleteForeverFlow: (refs: ThreadRef[]) => Promise<void>;
  navTo: (delta: number) => void;
  actOnOpen: (kind: "archive" | "trash" | "spam" | "unread" | "read") => void;
  run: (label: string, fn: () => Promise<unknown>) => Promise<void>;
  pickTheme: (id: string) => void;
  pickFont: (id: string) => void;
  toggleLabelHidden: (id: string) => void;
}

export type AppState = AppFields & FieldSetters<AppFields> & AppActions;
export type StoreActionKey = keyof AppActions;

