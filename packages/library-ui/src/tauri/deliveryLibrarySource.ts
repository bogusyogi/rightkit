// The `LibrarySource` over a dictation app's delivery history: the paged query, its change events, copy, delete, edit,
// export, retention, the recovery notice with its diagnostics export, and the app icons. The mapping rules are here;
// the command and event names, the row mapping, every word and every file name come in through the options, so the
// source holds no product name. Moved from HeardRight `lib/historySource.ts`.
//
//   import { deliveryLibrarySource } from "@rightkit/library-ui/tauri";
//   const source = deliveryLibrarySource({ invoke, row: historyRowOf, changeEvents, pickSavePath, ... });
import type { LibraryOrigin, LibraryPageResult, LibraryQuery, LibraryRecoveryStatus, LibraryRow, LibrarySource, LibraryRetention, Unsubscribe } from "../contract.js";
import { originExePath } from "../history/delivery-row.js";

/** The native command names; each can be overridden. */
export interface DeliveryLibraryCommands {
  /** Paged query, called with `{ query }` (see `DeliveryHistoryQuery`). */
  query: string;
  /** `{ text }` */
  copyText: string;
  /** `{ deliveryId }` */
  remove: string;
  /** `{ deliveryId, transcript }`; resolves false when the row no longer exists. */
  update: string;
  /** `{ path }`; resolves the number of exported rows. */
  exportAll: string;
  /** `{ path, contents }`: the generic text-file writer behind "Export selected". */
  writeText: string;
  getRetention: string;
  /** `{ retention }`; resolves the number of rows removed. */
  setRetention: string;
  /** Resolves a `DeliveryRecoveryStatus`, or null when nothing was set aside. */
  recoveryStatus: string;
  /** `{ path }` */
  exportRecoveryDiagnostics: string;
  /** `{ processName, exePath }`; resolves a `data:image/` URL or null. */
  appIcon: string;
}

const COMMANDS: DeliveryLibraryCommands = {
  query: "query_history",
  copyText: "copy_text_to_clipboard",
  remove: "delete_history_item",
  update: "update_history_item",
  exportAll: "export_history",
  writeText: "write_text_file",
  getRetention: "get_history_retention",
  setRetention: "set_history_retention",
  recoveryStatus: "get_history_recovery_status",
  exportRecoveryDiagnostics: "export_history_recovery_diagnostics",
  appIcon: "app_icon_png",
};

/** The wire form of a library query: what the native paged query takes. */
export interface DeliveryHistoryQuery {
  search: string;
  from_ms: number | null;
  until_ms: number | null;
  /** The origin filter's key (the raw app label), "" for all. */
  app: string;
  cancelled_only: boolean;
  offset: number;
  limit: number;
}

export interface DeliveryHistoryPage<Item> {
  items: Item[];
  total: number;
  has_more: boolean;
}

/** What the native store reports when it could not open the history database and set it aside. */
export interface DeliveryRecoveryStatus {
  reason: string;
  created_at_ms: number;
  state: string;
  backups: { path: string; bytes: number | null }[];
}

export interface SaveFileFilter {
  name: string;
  extensions: string[];
}

export interface DeliveryLibrarySourceOptions<Item> {
  /** The IPC call (`@tauri-apps/api/core` `invoke`, or the app's bridge to it). */
  invoke<T>(command: string, args?: Record<string, unknown>): Promise<T>;
  /** Overrides of the command names (defaults are the dictation host's). */
  commands?: Partial<DeliveryLibraryCommands>;
  /** The library row for a record (`deliveryRow` with the app's `DeliveryRowConfig`). */
  row(item: Item): LibraryRow;
  /**
   * The paged query. Default: `invoke(commands.query, { query })`. Supply it to route the call through the app's
   * own typed command client (its error mapping).
   */
  queryPage?(query: DeliveryHistoryQuery): Promise<DeliveryHistoryPage<Item>>;
  /** The variant key that means "cancelled only". Default `"cancelled"`; every other variant is the default scope. */
  cancelledVariant?: string;
  /**
   * One registration per native change event (a delivery, records synced in). A registration that fails does not
   * fail the subscription: a preview host with no events leaves the list as loaded.
   */
  changeEvents: readonly ((onChange: () => void) => Promise<Unsubscribe>)[];
  /** A save dialog (`@rightkit/tauri/dialog` `pickSavePath`): the chosen path, or null/undefined when cancelled. */
  pickSavePath(options: { defaultPath: string; filters: SaveFileFilter[] }): Promise<string | null | undefined>;
  /** "Export all": the suggested file name for `today` (`YYYY-MM-DD`, UTC) and the formats the native export writes. */
  exportAll: { fileName(today: string): string; filters: SaveFileFilter[] };
  /** "Export selected": the filters of the text save dialog. */
  saveText: { filters: SaveFileFilter[] };
  retention: { options: LibraryRetention["options"] };
  /** The notice shown when the store was set aside, and the diagnostics export's dialog. */
  recovery: { title: string; message: string; diagnosticsFileName: string; diagnosticsFilters: SaveFileFilter[] };
}

const todayStamp = () => new Date().toISOString().slice(0, 10);

export function deliveryLibrarySource<Item>(options: DeliveryLibrarySourceOptions<Item>): LibrarySource {
  const cmd = { ...COMMANDS, ...options.commands };
  const { invoke } = options;
  const queryPage =
    options.queryPage ?? ((query: DeliveryHistoryQuery) => invoke<DeliveryHistoryPage<Item>>(cmd.query, { query }));

  return {
    async query(query: LibraryQuery): Promise<LibraryPageResult> {
      const page = await queryPage({
        search: query.search,
        from_ms: query.fromMs,
        until_ms: query.untilMs,
        // The origin filter carries the app label; "cancelled" is the one variant (the default tab is the default scope).
        app: query.origin,
        cancelled_only: query.variant === (options.cancelledVariant ?? "cancelled"),
        offset: query.offset,
        limit: query.limit,
      });
      return { rows: page.items.map(options.row), total: page.total, hasMore: page.has_more };
    },
    async subscribe(onChange) {
      const settled = await Promise.allSettled(options.changeEvents.map((register) => register(onChange)));
      return () => {
        for (const registration of settled) if (registration.status === "fulfilled") registration.value();
      };
    },
    copyText: (text) => invoke<void>(cmd.copyText, { text }),
    remove: (id) => invoke<void>(cmd.remove, { deliveryId: id }),
    update: (id, text) => invoke<boolean>(cmd.update, { deliveryId: id, transcript: text }),
    async exportAll() {
      const destination = await options.pickSavePath({
        defaultPath: options.exportAll.fileName(todayStamp()),
        filters: options.exportAll.filters,
      });
      if (!destination) return null;
      return invoke<number>(cmd.exportAll, { path: destination });
    },
    async saveText(suggestedName, contents) {
      const destination = await options.pickSavePath({ defaultPath: suggestedName, filters: options.saveText.filters });
      if (!destination) return false;
      await invoke(cmd.writeText, { path: destination, contents });
      return true;
    },
    retention: {
      options: options.retention.options,
      get: () => invoke<string>(cmd.getRetention),
      set: (value) => invoke<number>(cmd.setRetention, { retention: value }),
    },
    recovery: {
      async status(): Promise<LibraryRecoveryStatus | null> {
        const status = await invoke<DeliveryRecoveryStatus | null>(cmd.recoveryStatus);
        if (!status || status.backups.length === 0) return null;
        return { createdAtMs: status.created_at_ms, title: options.recovery.title, message: options.recovery.message };
      },
      async exportDiagnostics() {
        const destination = await options.pickSavePath({
          defaultPath: options.recovery.diagnosticsFileName,
          filters: options.recovery.diagnosticsFilters,
        });
        if (!destination) return false;
        await invoke(cmd.exportRecoveryDiagnostics, { path: destination });
        return true;
      },
    },
    // The icon command resolves a process (by exe path when known, else by name) to a `data:image/png` URL.
    async originIcon(origin: LibraryOrigin) {
      const url = await invoke<string | null>(cmd.appIcon, {
        processName: origin.processName?.trim() ?? "",
        exePath: originExePath(origin)?.trim() || null,
      });
      return typeof url === "string" && url.startsWith("data:image/") ? url : null;
    },
  };
}
