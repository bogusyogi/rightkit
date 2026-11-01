import Icon from "@rightkit/agent-ui-core/components/Icon";
import type { TelegramRemoteUsers } from "@rightkit/agent-ui-core/lib/types";
import type { SettingsDraft } from "../../types.js";

export function TelegramUsersPanel({
  settingsDraft,
  telegramUsers,
  usersLoading,
  onSettingsDraft,
  onDiscoverUsers,
}: {
  settingsDraft: SettingsDraft;
  telegramUsers: TelegramRemoteUsers | null;
  usersLoading: boolean;
  onSettingsDraft: (draft: SettingsDraft) => void;
  onDiscoverUsers: () => void;
}) {
  const appendAllowedUser = (userId: number) => {
    const existing = settingsDraft.remote.telegramAllowedUsersText.trim();
    const values = existing
      ? existing
          .split(/[\s,]+/)
          .map((value) => value.trim())
          .filter(Boolean)
      : [];
    const next = String(userId);
    if (!values.includes(next)) values.push(next);
    onSettingsDraft({
      ...settingsDraft,
      remote: {
        ...settingsDraft.remote,
        telegramAllowedUsersText: values.join(", "),
      },
    });
  };
  return (
    <div className="flex flex-col gap-2 rounded-lg border border-line bg-panel-2 p-3 text-[12px] text-text-2">
      <div className="flex items-center justify-between gap-3">
        <div className="min-w-0">
          <div className="font-semibold text-text">Recent Telegram users</div>
          <div className="text-text-3">
            Save the token, message the bot once, then discover ids.
          </div>
        </div>
        <button
          type="button"
          className="inline-flex items-center gap-2 h-8 px-3 rounded-lg border border-line bg-panel text-text-2 text-[12px] cursor-pointer hover:bg-panel-2 hover:text-text disabled:opacity-40"
          disabled={usersLoading || settingsDraft.remote.clearTelegramToken}
          onClick={onDiscoverUsers}
        >
          <Icon name="activity" />
          {usersLoading ? "Finding" : "Find"}
        </button>
      </div>
      {telegramUsers?.users.length ? (
        <div className="divide-y divide-line rounded-lg border border-line overflow-hidden">
          {telegramUsers.users.map((user) => (
            <div
              className="grid grid-cols-[1fr_auto] gap-3 px-3 py-2"
              key={`${user.userId}-${user.lastUpdateId}`}
            >
              <div className="min-w-0">
                <div className="text-text font-mono text-[12px]">{user.userId}</div>
                <div className="text-text-3 truncate">
                  {[user.username ? `@${user.username}` : null, user.firstName, user.lastName]
                    .filter(Boolean)
                    .join(" ") || "Telegram user"}
                </div>
              </div>
              <button
                type="button"
                className="inline-flex items-center h-7 px-2 rounded-md border border-line bg-transparent text-text-2 text-[12px] cursor-pointer hover:bg-panel hover:text-text"
                onClick={() => appendAllowedUser(user.userId)}
              >
                Add
              </button>
            </div>
          ))}
        </div>
      ) : telegramUsers ? (
        <div className="text-text-3">
          No recent users found. Ask the intended operator to message the bot, then run discovery
          again.
        </div>
      ) : null}
    </div>
  );
}
