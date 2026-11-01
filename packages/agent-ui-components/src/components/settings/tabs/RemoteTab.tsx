import Icon from "@rightkit/agent-ui-core/components/Icon";
import { OfflinePanel } from "@rightkit/agent-ui-settings/components/settings/tabs/shared";
import type {
  RemoteChannelsSnapshot,
  TelegramRemoteStatus,
  TelegramRemoteTest,
  TelegramRemoteUsers,
} from "@rightkit/agent-ui-core/lib/types";
import type { SettingsDraft } from "../types";
import { RemoteChannelsPanel } from "@rightkit/agent-ui-settings/components/settings/tabs/remote/RemoteChannelsPanel";
import { TelegramSetupPanel } from "./remote/TelegramSetupPanel";
import { TelegramStatusPanel } from "@rightkit/agent-ui-settings/components/settings/tabs/remote/TelegramStatusPanel";
import { TelegramUsersPanel } from "./remote/TelegramUsersPanel";
import RemoteDevicesPanel from "@rightkit/agent-ui-settings/components/remote/RemoteDevicesPanel";
import RemoteHostsPanel from "@rightkit/agent-ui-settings/components/remote/RemoteHostsPanel";

export default function RemoteTab({
  settingsDraft,
  telegramStatus,
  telegramTest,
  telegramUsers,
  remoteChannels,
  loading,
  usersLoading,
  error,
  usersError,
  onSettingsDraft,
  onRefresh,
  onDiscoverUsers,
}: {
  settingsDraft: SettingsDraft | null;
  telegramStatus: TelegramRemoteStatus | null;
  telegramTest: TelegramRemoteTest | null;
  telegramUsers: TelegramRemoteUsers | null;
  remoteChannels: RemoteChannelsSnapshot | null;
  loading: boolean;
  usersLoading: boolean;
  error: string | null;
  usersError: string | null;
  onSettingsDraft: (draft: SettingsDraft) => void;
  onRefresh: () => void;
  onDiscoverUsers: () => void;
}) {
  return (
    <section className="flex flex-col gap-5 p-6">
      <div className="flex items-center gap-3">
        <div className="flex flex-col gap-1 flex-1 min-w-0">
          <h3 className="text-[16px] font-bold text-text">Remote</h3>
          <p className="text-[13px] text-text-2">
            Telegram remote control status, setup readiness, and session binding storage.
          </p>
        </div>
        <button
          type="button"
          className="inline-flex items-center gap-2 h-8 px-3 rounded-lg border border-line bg-transparent text-text-2 text-[13px] cursor-pointer hover:bg-panel hover:text-text disabled:opacity-40"
          disabled={loading}
          onClick={onRefresh}
        >
          <Icon name="activity" />
          {loading ? "Refreshing" : "Refresh"}
        </button>
      </div>

      {error ? <OfflinePanel title="Remote status unavailable." action={error} /> : null}
      {usersError ? (
        <OfflinePanel title="Telegram user discovery unavailable." action={usersError} />
      ) : null}

      <RemoteChannelsPanel snapshot={remoteChannels} />

      {settingsDraft ? (
        <>
          <TelegramSetupPanel settingsDraft={settingsDraft} onSettingsDraft={onSettingsDraft} />
          <div className="grid grid-cols-1 xl:grid-cols-2 gap-4">
            <TelegramUsersPanel
              settingsDraft={settingsDraft}
              telegramUsers={telegramUsers}
              usersLoading={usersLoading}
              onSettingsDraft={onSettingsDraft}
              onDiscoverUsers={onDiscoverUsers}
            />
          </div>
        </>
      ) : null}

      <TelegramStatusPanel
        telegramStatus={telegramStatus}
        telegramTest={telegramTest}
        error={error}
      />
      <RemoteDevicesPanel open />
      <RemoteHostsPanel open />
    </section>
  );
}
