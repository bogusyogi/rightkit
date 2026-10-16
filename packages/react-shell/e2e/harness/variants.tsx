// Drives the opt-in 0.6.0 options. Every option comes from the query string so one page covers each
// combination; with no options the components render exactly as in 0.5.x (pinned elsewhere).
import { useState, type ReactNode } from "react";
import { createRoot } from "react-dom/client";
import {
  ActionDialog,
  CommandPalette,
  ContextMenu,
  useContextMenu,
  ModalShell,
  ToastViewport,
  createToastBus,
  rankCommands,
  type Command,
  type MenuItem,
} from "../../src/react.js";
import "../../shell.css";

const q = new URLSearchParams(location.search);
const demo = q.get("demo") ?? "menu";
const num = (k: string) => (q.has(k) ? Number(q.get(k)) : undefined);
const log = (line: string) => {
  const out = document.getElementById("log");
  if (out) out.textContent += `${line}\n`;
};

function MenuDemo() {
  const [at, setAt] = useState<{ x: number; y: number; items: MenuItem[] } | null>(null);
  const makeItems = (): MenuItem[] => [
    { id: "cut", label: "Cut", disabled: true, run: () => log("menu:cut") },
    { id: "copy", label: "Copy", shortcut: "mod+c", icon: <b>c</b>, run: () => log("menu:copy") },
    { id: "paste", label: "Paste", run: () => log("menu:paste") },
    { id: "all", label: "Select all", separatorBefore: true, run: () => log("menu:all") },
  ];
  return (
    <div style={{ padding: 24 }}>
      <input id="field" defaultValue="hello world" />
      <button type="button" id="page-btn" onClick={() => log("page:btn")}>page button</button>
      <div id="area" style={{ marginTop: 20, height: 200, background: "#eee" }} onContextMenu={(e) => { e.preventDefault(); setAt({ x: e.clientX, y: e.clientY, items: makeItems() }); }}>right-click</div>
      {at ? (
        <ContextMenu
          x={at.x}
          y={at.y}
          items={at.items}
          label="Edit"
          onClose={() => { log("menu:closed"); setAt(null); }}
          itemAs={(q.get("itemAs") as "div" | "button" | null) ?? undefined}
          showLead={q.get("lead") === "0" ? false : undefined}
          scrimClassName={q.get("scrim") ?? undefined}
          autoFocus={q.get("autoFocus") === "0" ? false : undefined}
          initialHighlight={(q.get("highlight") as "first" | "none" | null) ?? undefined}
          keyboard={(q.get("keyboard") as "menu" | "escape-only" | null) ?? undefined}
          exitMs={num("exit")}
          exitClassName={q.get("exitClass") ?? undefined}
          onExitStart={q.has("exit") ? () => log("menu:exit-start") : undefined}
          renderItem={q.get("render") ? (item, { active }) => <i className="custom-row" data-active={active}>{item.label}</i> : undefined}
        />
      ) : null}
    </div>
  );
}

function HookMenuDemo() {
  const menu = useContextMenu<undefined>(() => [{ id: "a", label: "Alpha", run: () => log("hook:alpha") }, { id: "b", label: "Beta", run: () => log("hook:beta") }], "Hook", { exitMs: 250, onExitStart: () => log("hook:exit") });
  return (
    <div id="area" style={{ margin: 24, height: 200, background: "#eee" }} onContextMenu={(e) => menu.open(e)}>right-click{menu.element}</div>
  );
}

function ModalDemo() {
  const [open, setOpen] = useState(true);
  if (!open) return <div id="closed">closed</div>;
  return (
    <ModalShell
      label="Plain"
      onClose={() => { log("modal:closed"); setOpen(false); }}
      dismissOn={(q.get("dismissOn") as "mousedown" | "click" | null) ?? undefined}
      overlayClassName={q.get("overlayClass") ?? undefined}
      overlayStyle={q.get("overlayBg") ? { background: q.get("overlayBg")! } : undefined}
      ariaLabel={q.get("ariaLabel") ?? undefined}
      labelledBy={q.get("ariaLabel") ? "lbl" : undefined}
    >
      <h2 id="lbl">Heading</h2>
      <button type="button" id="inside">inside</button>
    </ModalShell>
  );
}

function ActionDemo() {
  const [open, setOpen] = useState(true);
  if (!open) return <div id="closed">closed</div>;
  return (
    <ActionDialog
      title="Save changes?"
      description="Your edits will be lost."
      role={(q.get("role") as "dialog" | "alertdialog" | null) ?? undefined}
      onClose={() => { log("action:closed"); setOpen(false); }}
      actions={[
        { id: "dont", label: "Don't save", variant: "danger", className: "act-dont", onClick: () => log("action:dont") },
        { id: "cancel", label: "Cancel", onClick: () => { log("action:cancel"); setOpen(false); } },
        { id: "save", label: "Save", variant: "primary", className: "act-save", autoFocus: q.get("focus") !== "0", onClick: () => log("action:save") },
      ]}
    />
  );
}

const commands: Command[] = [
  { id: "open", label: "Open file", group: "File", run: () => log("cmd:open") },
  { id: "save", label: "Save file", group: "File", run: () => log("cmd:save") },
  { id: "theme", label: "Toggle theme", group: "View", icon: <b>t</b>, run: () => log("cmd:theme") },
];

function PaletteDemo() {
  const [open, setOpen] = useState(true);
  if (!open) return <div id="closed">closed</div>;
  return (
    <CommandPalette
      commands={commands}
      onClose={() => { log("palette:closed"); setOpen(false); }}
      labels={q.get("inputAria") ? { inputAriaLabel: q.get("inputAria")! } : undefined}
      enterDelayMs={num("delay")}
      rank={q.get("rank") === "reverse" ? (list, query, limit) => [...rankCommands(list, query, limit)].reverse() : q.get("rank") === "only-theme" ? (list) => list.filter((c) => c.id === "theme") : undefined}
      searchIcon={q.get("searchIcon") === "none" ? null : q.get("searchIcon") ? <span id="my-search">?</span> : undefined}
      renderIcon={q.get("renderIcon") ? (c, { active }) => <u className="my-icon" data-active={active}>{c.id}</u> : undefined}
      slots={q.get("iconClass") ? { icon: q.get("iconClass")! } : undefined}
    />
  );
}

const bus = createToastBus();
(window as unknown as { __bus: typeof bus }).__bus = bus;

function ToastDemo() {
  const [exit, setExit] = useState(num("exit"));
  (window as unknown as { __setExit: (n: number) => void }).__setExit = setExit;
  let glyph: ((t: { kind: string }) => ReactNode) | undefined;
  if (q.get("glyph")) glyph = (t) => <svg className="my-glyph" data-kind={t.kind} width="10" height="10" />;
  return (
    <ToastViewport
      bus={bus}
      roles={q.get("roles") === "vr" ? { error: "alert", info: null, success: null, warning: null } : q.get("roles") === "log" ? { success: "log" } : undefined}
      rowClassName={q.get("rowClass") ? (t, { leaving }) => `row-${t.kind}${leaving ? " row-leaving" : ""}` : undefined}
      exitClassName={q.get("exitClass") ?? undefined}
      exitMs={exit}
      renderGlyph={glyph as never}
      renderRow={q.get("row") ? ({ toast, role, className, dismiss, leaving }) => <div className={`${className} my-row`} role={role} data-leaving={leaving} onClick={dismiss}>{toast.message}</div> : undefined}
    />
  );
}

createRoot(document.getElementById("root")!).render(
  <>
    {demo === "hook" ? <HookMenuDemo /> : demo === "menu" ? <MenuDemo /> : demo === "modal" ? <ModalDemo /> : demo === "action" ? <ActionDemo /> : demo === "palette" ? <PaletteDemo /> : <ToastDemo />}
    <pre id="log" style={{ position: "fixed", left: -9999, top: 0 }} />
  </>,
);
(window as unknown as { __ready: boolean }).__ready = true;
