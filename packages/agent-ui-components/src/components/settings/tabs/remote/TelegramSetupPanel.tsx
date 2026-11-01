import { Checkbox } from "@rightkit/react-shell/react";
import type { SettingsDraft } from "../../types";

export function TelegramSetupPanel({
  settingsDraft,
  onSettingsDraft,
}: {
  settingsDraft: SettingsDraft;
  onSettingsDraft: (draft: SettingsDraft) => void;
}) {
  return (
    <div className="rounded-xl border border-line bg-panel overflow-hidden">
      <div className="px-4 py-3 border-b border-line">
        <h4 className="text-[13px] font-semibold text-text">Telegram setup</h4>
        <p className="text-[12px] text-text-3">
          Save a bot token and allowed Telegram user ids. Restart the daemon after saving to start
          or refresh the long-poll task.
        </p>
      </div>
      <div className="flex flex-col gap-4 p-4">
        <div className="grid grid-cols-1 md:grid-cols-4 gap-2">
          {[
            ["1", "BotFather token", "Create a bot and paste its token."],
            ["2", "Message bot", "Send one Telegram message from the operator account."],
            ["3", "Find user id", "Run discovery and add the numeric Telegram id."],
            ["4", "Restart and smoke", "Restart, refresh readiness, then send a live message."],
          ].map(([step, title, detail]) => (
            <div key={step} className="flex gap-2 rounded-lg border border-line bg-panel-2 p-3">
              <span className="flex h-6 w-6 shrink-0 items-center justify-center rounded-full border border-line text-[11px] font-semibold text-text-2">
                {step}
              </span>
              <div className="min-w-0">
                <div className="text-[12px] font-semibold text-text">{title}</div>
                <div className="text-[11px] leading-relaxed text-text-3">{detail}</div>
              </div>
            </div>
          ))}
        </div>
        <div className="grid grid-cols-1 xl:grid-cols-2 gap-4">
          <label className="flex flex-col gap-2 text-[12px] text-text-2">
            Bot token
            <input
              className="h-9 rounded-lg border border-line bg-panel-2 px-3 text-[13px] text-text outline-none"
              type="password"
              placeholder={
                settingsDraft.remote.telegramTokenConfigured
                  ? "Configured - leave blank to keep"
                  : "Paste BotFather token"
              }
              value={settingsDraft.remote.telegramToken}
              disabled={settingsDraft.remote.clearTelegramToken}
              onChange={(event) =>
                onSettingsDraft({
                  ...settingsDraft,
                  remote: {
                    ...settingsDraft.remote,
                    telegramToken: event.target.value,
                  },
                })
              }
            />
          </label>
          <label className="flex flex-col gap-2 text-[12px] text-text-2">
            Allowed user ids
            <textarea
              className="min-h-[78px] rounded-lg border border-line bg-panel-2 px-3 py-2 text-[13px] text-text outline-none resize-y"
              placeholder="123456789, 987654321"
              value={settingsDraft.remote.telegramAllowedUsersText}
              onChange={(event) =>
                onSettingsDraft({
                  ...settingsDraft,
                  remote: {
                    ...settingsDraft.remote,
                    telegramAllowedUsersText: event.target.value,
                  },
                })
              }
            />
          </label>
          <Checkbox checked={settingsDraft.remote.clearTelegramToken} onChange={(checked) => onSettingsDraft({
                  ...settingsDraft,
                  remote: {
                    ...settingsDraft.remote,
                    clearTelegramToken: checked,
                    telegramToken: checked ? "" : settingsDraft.remote.telegramToken,
                  },
                })} label="Clear saved Telegram token" className="flex items-center gap-2 text-[12px] text-text-2" />
          <Checkbox
            checked={settingsDraft.remote.webEnabled}
            onChange={(checked) =>
              onSettingsDraft({
                ...settingsDraft,
                remote: {
                  ...settingsDraft.remote,
                  webEnabled: checked,
                },
              })
            }
            label={
              <>
                Enable remote web access flag
                {settingsDraft.remote.webEnabledSource ? (
                  <span
                    className="rounded-full border border-line bg-panel-2 px-2 py-0.5 text-[10px] font-medium text-text-3"
                    title={`Source: ${settingsDraft.remote.webEnabledSource}`}
                  >
                    {settingsDraft.remote.webEnabledSource}
                  </span>
                ) : null}
              </>
            }
            className="flex items-center gap-2 text-[12px] text-text-2"
          />
        </div>
      </div>
    </div>
  );
}
