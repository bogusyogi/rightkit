import { create } from "zustand";

import {
  getRemoteChannels,
  getTelegramStatus,
  getTelegramTest,
  getTelegramUsers,
} from "../lib/daemon";
import type {
  DaemonConfig,
  RemoteChannelsSnapshot,
  TelegramRemoteStatus,
  TelegramRemoteTest,
  TelegramRemoteUsers,
} from "../lib/types";
import { messageOf } from "../lib/utils";

/** Telegram / remote-channels settings: status, test, users, channel snapshot. */
export type TelegramStoreState = {
  status: TelegramRemoteStatus | null;
  test: TelegramRemoteTest | null;
  users: TelegramRemoteUsers | null;
  remoteChannels: RemoteChannelsSnapshot | null;
  statusLoading: boolean;
  usersLoading: boolean;
  statusError: string | null;
  usersError: string | null;
  loadStatus: (config: DaemonConfig | null) => Promise<void>;
  loadUsers: (config: DaemonConfig | null) => Promise<void>;
};

export const useTelegramStore = create<TelegramStoreState>((set) => ({
  status: null,
  test: null,
  users: null,
  remoteChannels: null,
  statusLoading: false,
  usersLoading: false,
  statusError: null,
  usersError: null,

  async loadStatus(config) {
    if (!config) return;
    set({ statusLoading: true, statusError: null });
    try {
      const [status, test, channels] = await Promise.all([
        getTelegramStatus(config),
        getTelegramTest(config),
        getRemoteChannels(config),
      ]);
      set({ status, test, remoteChannels: channels });
    } catch (err) {
      set({ statusError: messageOf(err, "Telegram status failed") });
    } finally {
      set({ statusLoading: false });
    }
  },

  async loadUsers(config) {
    if (!config) return;
    set({ usersLoading: true, usersError: null });
    try {
      const users = await getTelegramUsers(config);
      set({ users });
      if (users.verdict === "block") {
        set({ usersError: users.detail ?? "Telegram user discovery blocked" });
      }
    } catch (err) {
      set({ usersError: messageOf(err, "Telegram users failed") });
    } finally {
      set({ usersLoading: false });
    }
  },
}));
