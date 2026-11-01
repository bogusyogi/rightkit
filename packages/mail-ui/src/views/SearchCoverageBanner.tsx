import { Button } from "@rightkit/react-shell/react";
import type { ArchiveProgress } from "../lib/ipc";
import { searchCoverageBannerText } from "../lib/searchCoverage";

export function SearchCoverageBanner({
  coverage,
  onDismiss,
}: {
  coverage: ArchiveProgress | null;
  onDismiss: () => void;
}) {
  const text = searchCoverageBannerText(coverage);
  if (!text) return null;
  return (
    <div className="folder-banner">
      <span>{text}</span>
      <Button variant="ghost" className="banner-link" onClick={onDismiss}>
        Dismiss
      </Button>
    </div>
  );
}
