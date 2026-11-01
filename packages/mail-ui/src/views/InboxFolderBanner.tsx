import { Button } from "@rightkit/react-shell/react";
export function InboxFolderBanner({
  destructiveOn,
  emptyFolderFlow,
  folder,
  listLength,
}: {
  destructiveOn: boolean;
  emptyFolderFlow: (label: "TRASH" | "SPAM", count: number) => void;
  folder: string;
  listLength: number;
}) {
  if (folder !== "trash" && folder !== "spam") return null;
  const isTrash = folder === "trash";
  return (
    <div className="folder-banner">
      <span>
        Messages that have been in {isTrash ? "Trash" : "Spam"} more than 30
        days will be automatically deleted.
      </span>
      <Button variant="ghost"
        className="banner-link"
        title={
          destructiveOn
            ? isTrash
              ? "Permanently delete everything in Trash"
              : "Permanently delete everything in Spam"
            : "Permanent delete is off (safety) - this will be refused until enabled"
        }
        onClick={() => emptyFolderFlow(isTrash ? "TRASH" : "SPAM", listLength)}
      >
        {isTrash ? "Empty Trash now" : "Delete all spam messages now"}
      </Button>
    </div>
  );
}
