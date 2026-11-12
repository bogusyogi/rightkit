// Settings · Agents — the agent activity log.
import { SettingRow, SettingsGroup } from "@rightkit/settings-ui/core";
import { Button, Table, TableBody, TableCell, TableHead, TableHeaderCell, TableRow } from "@rightkit/react-shell/react";
import type { AgentAuditRow } from "@rightkit/mail-ui/lib/ipc";
import { auditScope } from "../lib/agentWire.js";

function when(ms: number): string {
  return new Date(ms).toLocaleString();
}

export function SettingsAgentsAudit({
  rows,
  more,
  onLoadMore,
}: {
  rows: AgentAuditRow[];
  more: boolean;
  onLoadMore: () => void;
}) {
  return (
    <SettingsGroup title="Agent activity" description={rows.length === 0 ? "No activity yet." : undefined}>
      {rows.length > 0 && (
        <SettingRow id="agents.activity" label="Activity log" keywords={["audit", "history"]} stacked>
          <Table label="Agent activity log" style={{ width: "100%" }}>
            <TableHead>
              <TableRow>
                <TableHeaderCell>Time</TableHeaderCell>
                <TableHeaderCell>Agent</TableHeaderCell>
                <TableHeaderCell>Account</TableHeaderCell>
                <TableHeaderCell>Action</TableHeaderCell>
                <TableHeaderCell>Result</TableHeaderCell>
                <TableHeaderCell>Details</TableHeaderCell>
              </TableRow>
            </TableHead>
            <TableBody>
              {rows.map((r) => (
                <TableRow key={r.id}>
                  <TableCell>{when(r.at)}</TableCell>
                  <TableCell>{r.agent_name ?? ""}</TableCell>
                  <TableCell>{auditScope(r) ?? ""}</TableCell>
                  <TableCell>{r.method}</TableCell>
                  <TableCell>{r.outcome}</TableCell>
                  <TableCell>{r.detail ?? ""}</TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </SettingRow>
      )}
      {more && (
        <SettingRow id="agents.more" label="Older activity">
          <Button size="sm" variant="secondary" onClick={onLoadMore}>
            Load more
          </Button>
        </SettingRow>
      )}
    </SettingsGroup>
  );
}
