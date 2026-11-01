import Icon from "@rightkit/agent-ui-core/components/Icon";
import { hostAppName } from "../../appName";

export default function SessionEmptyState({
  workspaceRoot,
  onOpenWorkspace,
  onPrefillComposer,
}: {
  workspaceRoot: string;
  onOpenWorkspace: () => void;
  onPrefillComposer: (text: string) => void;
}) {
  if (!workspaceRoot.trim()) {
    return (
      <div className="flex flex-col gap-4 m-auto text-center max-w-[380px] py-10">
        <div className="flex flex-col gap-2">
          <h2 className="text-[17px] font-semibold text-text">Open a project to begin</h2>
          <p className="text-[13px] text-text-3 leading-relaxed">
            Pick a folder and {hostAppName()} attaches the workspace, branch, and run target to this
            session.
          </p>
        </div>
        <button
          type="button"
          className="inline-flex items-center gap-2 mx-auto px-4 py-2 rounded-lg border-0 bg-accent text-on-accent text-[13px] font-semibold cursor-pointer hover:opacity-90"
          onClick={onOpenWorkspace}
        >
          <Icon name="folder" size={14} />
          Choose project
        </button>
      </div>
    );
  }

  return (
    <div className="flex flex-col gap-5 m-auto max-w-[420px] py-10">
      <div className="flex flex-col gap-2 text-center">
        <h2 className="text-[17px] font-semibold text-text">What should {hostAppName()} do?</h2>
        <p className="text-[13px] text-text-3 leading-relaxed">
          Start with a concrete task or question.
        </p>
      </div>
      <div className="grid gap-2">
        {[
          "Write tests for the current file",
          "Fix the TypeScript errors in this project",
          "Refactor this module to reduce complexity",
        ].map((prompt) => (
          <button
            key={prompt}
            type="button"
            className="text-left px-4 py-3 rounded-lg border border-line bg-panel text-[13px] text-text-2 hover:border-line-2 hover:text-text cursor-pointer transition-colors"
            onClick={() => onPrefillComposer(prompt)}
          >
            {prompt}
          </button>
        ))}
      </div>
    </div>
  );
}
