import { open } from "@tauri-apps/plugin-dialog";
import { useMemo, useState } from "react";
import {
  groupProjectRecents,
  normalizeProjectPath,
  useProjectRecentsStore,
} from "@rightkit/agent-ui-core/state/projectRecentsStore";

export interface ProjectPickerProps {
  /** The workspace currently displayed by the app shell. */
  workspaceRoot: string;
  /** The hub owns session creation and applies the selected workspace. */
  onWorkspaceChange: (path: string) => void;
  onClose?: () => void;
  disabled?: boolean;
}

/**
 * Project entry surface. The native directory dialog is deliberately opened
 * here, rather than through a renderer-side filesystem implementation: this
 * is the direct React-side port of Aethon's `pick_project_directory` seam.
 * Recents are a local convenience cache until the daemon project registry
 * endpoint exists.
 */
export default function ProjectPicker({
  workspaceRoot,
  onWorkspaceChange,
  onClose,
  disabled = false,
}: ProjectPickerProps) {
  const projects = useProjectRecentsStore((state) => state.projects);
  const activePath = useProjectRecentsStore((state) => state.activePath);
  const addOrTouch = useProjectRecentsStore((state) => state.addOrTouch);
  const remove = useProjectRecentsStore((state) => state.remove);
  const togglePinned = useProjectRecentsStore((state) => state.togglePinned);
  const setGroup = useProjectRecentsStore((state) => state.setGroup);
  const [opening, setOpening] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [typedPath, setTypedPath] = useState("");

  const groups = useMemo(() => groupProjectRecents(projects), [projects]);
  const active = normalizeProjectPath(activePath ?? workspaceRoot);

  async function openProjectDirectory() {
    if (opening || disabled) return;
    setOpening(true);
    setError(null);
    try {
      const picked = await open({
        directory: true,
        multiple: false,
        title: "Choose project directory",
      });
      if (typeof picked !== "string" || !picked.trim()) return;
      selectProject(picked);
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      setOpening(false);
    }
  }

  /**
   * Path-entry route: no native dialog, so hidden/headless runs (QA journey 3)
   * and keyboard users can open a folder by typing its path.
   */
  function openTypedPath() {
    if (disabled) return;
    const path = typedPath.trim();
    if (!path) {
      setError("Enter a project folder path.");
      return;
    }
    setError(null);
    selectProject(path);
    setTypedPath("");
  }

  function selectProject(path: string) {
    const project = addOrTouch(path);
    if (!project) return;
    onWorkspaceChange(project.path);
  }

  return (
    <section aria-label="Projects" data-testid="project-picker" className="flex flex-col gap-2">
      <header className="flex items-center justify-between gap-2">
        <div className="min-w-0">
          <h2 className="text-[13px] font-medium text-text">Projects</h2>
          <p className="truncate font-mono text-[10px] text-text-3" title={workspaceRoot}>
            {workspaceRoot || "No workspace selected"}
          </p>
        </div>
        {onClose ? (
          <button
            type="button"
            className="rounded px-2 py-1 text-[11px] text-text-2 hover:bg-panel-2 hover:text-text"
            onClick={onClose}
            aria-label="Close project picker"
          >
            Close
          </button>
        ) : null}
      </header>

      <button
        type="button"
        onClick={() => void openProjectDirectory()}
        disabled={disabled || opening}
        className="rounded border border-line bg-panel-2 px-3 py-2 text-left text-[12px] text-text hover:border-accent disabled:cursor-not-allowed disabled:opacity-50"
        data-testid="project-picker-open"
      >
        {opening ? "Choosing project…" : "Open project folder…"}
      </button>

      <form
        className="flex gap-2"
        onSubmit={(event) => {
          event.preventDefault();
          openTypedPath();
        }}
      >
        <label className="sr-only" htmlFor="project-picker-path">
          Project folder path
        </label>
        <input
          id="project-picker-path"
          data-testid="project-picker-path"
          type="text"
          value={typedPath}
          onChange={(event) => setTypedPath(event.target.value)}
          placeholder="Or type a folder path"
          disabled={disabled}
          className="min-w-0 flex-1 rounded border border-line bg-panel-2 px-2 py-1 font-mono text-[11px] text-text"
        />
        <button
          type="submit"
          disabled={disabled}
          data-testid="project-picker-path-open"
          className="rounded border border-line bg-panel-2 px-3 py-1 text-[12px] text-text hover:border-accent disabled:cursor-not-allowed disabled:opacity-50"
        >
          Open path
        </button>
      </form>

      {error ? (
        <p role="alert" className="text-[11px] text-danger" data-testid="project-picker-error">
          {error}
        </p>
      ) : null}

      {groups.length === 0 ? (
        <p className="px-1 py-2 text-[11px] text-text-3" data-testid="project-picker-empty">
          No recent projects yet.
        </p>
      ) : (
        <div className="flex max-h-[360px] flex-col gap-3 overflow-y-auto" data-testid="project-picker-groups">
          {groups.map((group) => (
            <section key={group.name} aria-label={group.name}>
              <h3 className="px-1 pb-1 text-[10px] font-medium uppercase tracking-wide text-text-3">
                {group.name}
              </h3>
              <ul className="flex flex-col gap-1">
                {group.projects.map((project) => {
                  const selected = project.path === active;
                  return (
                    <li key={project.path}>
                      <div
                        className={`flex items-center gap-2 rounded px-2 py-1.5 ${
                          selected ? "bg-accent/15" : "hover:bg-panel-2"
                        }`}
                        data-active={selected ? "true" : "false"}
                      >
                        <button
                          type="button"
                          onClick={() => selectProject(project.path)}
                          className="flex min-w-0 flex-1 items-center gap-2 text-left text-[12px] text-text"
                          aria-current={selected ? "true" : undefined}
                          title={project.path}
                        >
                          <span aria-hidden className="text-text-3">
                            {project.iconUrl ? "◆" : "▰"}
                          </span>
                          <span className="min-w-0 flex-1 truncate">{project.label}</span>
                        </button>
                        <button
                          type="button"
                          onClick={() => togglePinned(project.path)}
                          className="rounded px-1 text-[12px] text-text-3 hover:text-accent"
                          aria-label={`${project.pinned ? "Unpin" : "Pin"} ${project.label}`}
                          aria-pressed={project.pinned}
                        >
                          {project.pinned ? "★" : "☆"}
                        </button>
                        <button
                          type="button"
                          onClick={() => remove(project.path)}
                          className="rounded px-1 text-[12px] text-text-3 hover:text-danger"
                          aria-label={`Remove ${project.label} from recents`}
                        >
                          ×
                        </button>
                      </div>
                      <label className="sr-only" htmlFor={`project-group-${project.path}`}>
                        Group {project.label}
                      </label>
                      <input
                        id={`project-group-${project.path}`}
                        aria-label={`Group ${project.label}`}
                        value={project.group ?? ""}
                        onChange={(event) => setGroup(project.path, event.target.value)}
                        placeholder="Group"
                        className="ml-8 mt-0.5 w-[calc(100%-2rem)] border-b border-transparent bg-transparent px-1 text-[10px] text-text-3 outline-none focus:border-line"
                      />
                    </li>
                  );
                })}
              </ul>
            </section>
          ))}
        </div>
      )}
    </section>
  );
}

export { openProjectDirectoryForPicker };

/** Small exported seam for callers that only need Aethon's picker behavior. */
async function openProjectDirectoryForPicker(): Promise<string | null> {
  const picked = await open({ directory: true, multiple: false, title: "Choose project directory" });
  return typeof picked === "string" && picked.trim() ? picked : null;
}
