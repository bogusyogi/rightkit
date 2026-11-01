import { EmptyState } from "@rightkit/react-shell/react";
import { useState } from "react";
import type { TeamDefinition } from "@rightkit/agent-ui-core/lib/types";
import { formatTeamPresets } from "./runtimeUtils";
import { ConfirmDialog } from "@rightkit/react-shell/react";
import Icon from "@rightkit/agent-ui-core/components/Icon";

export default function TeamsRuntimePanel({
  teams,
  teamStorePath,
  pendingName,
  runPendingName,
  selectedSessionId,
  onDelete,
  onRun,
}: {
  teams: TeamDefinition[];
  teamStorePath: string;
  pendingName: string | null;
  runPendingName: string | null;
  selectedSessionId?: string | null;
  onDelete: (name: string) => void;
  onRun: (name: string, goal: string) => void;
}) {
  const [expandedTeam, setExpandedTeam] = useState<string | null>(null);
  const [goalDrafts, setGoalDrafts] = useState<Record<string, string>>({});
  const [pendingDelete, setPendingDelete] = useState<string | null>(null);

  if (teams.length === 0) {
    return (
      <EmptyState icon={<Icon name="user" size={18} />} title="No teams yet">{"Agent-created teams appear here after `team_create` runs. Sub-agents register teammates to coordinate work."}</EmptyState>
    );
  }

  return (
    <div className="flex-1 min-h-0 overflow-y-auto">
      <div className="px-5 py-3 border-b border-line text-[12px] text-text-3 truncate">
        {teamStorePath}
      </div>

      <div className="flex flex-col divide-y divide-line">
        {teams.map((team) => (
          <div
            className="grid grid-cols-[minmax(0,1fr)_auto_auto] gap-3 px-5 py-3 items-center"
            key={team.name}
          >
            <div className="min-w-0">
              <strong className="block text-[13px] font-semibold text-text truncate">
                {team.name}
              </strong>
              <p className="text-[12px] text-text-3 truncate">
                {team.description || "No description"}
              </p>
              <p className="text-[11px] text-text-3 truncate">{team.members.join(", ")}</p>
              {formatTeamPresets(team) ? (
                <p className="text-[11px] text-text-3 truncate">{formatTeamPresets(team)}</p>
              ) : null}
            </div>

            <button
              type="button"
              className="inline-flex items-center justify-center h-8 w-8 rounded-lg border border-line bg-panel text-text-2 hover:text-accent hover:border-accent cursor-pointer disabled:opacity-50"
              disabled={!selectedSessionId || runPendingName === team.name}
              onClick={() =>
                setExpandedTeam((current) => (current === team.name ? null : team.name))
              }
              title={`Run team ${team.name}`}
              aria-label={`Run team ${team.name}`}
            >
              <Icon name="play" size={14} />
            </button>

            <button
              type="button"
              className="inline-flex items-center justify-center h-8 w-8 rounded-lg border border-line bg-panel text-text-2 hover:text-danger hover:border-danger cursor-pointer disabled:opacity-50"
              disabled={pendingName === team.name}
              onClick={() => setPendingDelete(team.name)}
              title={`Delete team ${team.name}`}
              aria-label={`Delete team ${team.name}`}
            >
              <Icon name="trash" size={14} />
            </button>

            {expandedTeam === team.name ? (
              <div className="col-span-3 grid grid-cols-[minmax(0,1fr)_auto] gap-2 rounded-lg border border-line bg-panel-2 p-3">
                <textarea
                  className="min-h-[70px] rounded-lg border border-line bg-panel px-3 py-2 text-[12px] text-text outline-none resize-y focus:border-focus"
                  value={goalDrafts[team.name] ?? ""}
                  onChange={(e) =>
                    setGoalDrafts((drafts) => ({
                      ...drafts,
                      [team.name]: e.target.value,
                    }))
                  }
                  placeholder="team goal"
                  aria-label={`Goal for team ${team.name}`}
                />
                <button
                  type="button"
                  className="inline-flex items-center justify-center h-8 px-3 rounded-lg border-0 bg-accent text-on-accent text-[12px] font-semibold cursor-pointer hover:opacity-90 disabled:cursor-not-allowed disabled:border disabled:border-line disabled:bg-panel-2 disabled:text-text-3 self-start"
                  disabled={
                    !selectedSessionId ||
                    runPendingName === team.name ||
                    !(goalDrafts[team.name] ?? "").trim()
                  }
                  onClick={() => {
                    onRun(team.name, (goalDrafts[team.name] ?? "").trim());
                    setExpandedTeam(null);
                  }}
                >
                  {runPendingName === team.name ? "Running" : "Run"}
                </button>
              </div>
            ) : null}
          </div>
        ))}
      </div>
      {pendingDelete !== null ? (
        <ConfirmDialog
          title="Delete team?"
          description={`Delete team "${pendingDelete ?? ""}"? This can't be undone.`}
          confirmLabel="Delete"
          danger
          onCancel={() => setPendingDelete(null)}
          onConfirm={() => {
            if (pendingDelete) onDelete(pendingDelete);
            setPendingDelete(null);
          }}
        />
      ) : null}
    </div>
  );
}
