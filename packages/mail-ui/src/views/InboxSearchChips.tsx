import { Button } from "@rightkit/react-shell/react";
export function InboxSearchChips({
  hasOp,
  query,
  searching,
  toggleOp,
}: {
  hasOp: (op: string) => boolean;
  query: string;
  searching: boolean;
  toggleOp: (op: string) => void;
}) {
  if (!query.trim()) return null;
  return (
    <div className="search-chips">
      {(
        [
          ["has:attachment", "Has attachment"],
          ["is:unread", "Unread"],
          ["is:starred", "Starred"],
        ] as const
      ).map(([op, label]) => (
        <Button variant="ghost"
          key={op}
          className={`chip ${hasOp(op) ? "on" : ""}`}
          onClick={() => toggleOp(op)}
        >
          {label}
        </Button>
      ))}
      <span className="chip-hint">
        {searching ? (
          "Searching..."
        ) : (
          <>
            Try <code>from:</code> <code>to:</code> <code>subject:</code>
          </>
        )}
      </span>
    </div>
  );
}
