import { StrictMode, useState } from "react";
import { createRoot } from "react-dom/client";
import {
  AppFrame,
  AppShell,
  Badge,
  Button,
  Card,
  ContextMenu,
  EmptyState,
  Input,
  NavList,
  PageShell,
  Pill,
  Plane,
  SegmentedControl,
  ShellProvider,
  StatusRow,
  Titlebar,
  Toggle,
  createThemeStore,
  useCommands,
  useContextMenu,
  useShell,
  useShortcuts,
  useToast,
  type MenuItem,
  type NavGroup,
} from "../../src/react.js";
import "../../shell.css";

const q = new URLSearchParams(location.search);
const platform = (q.get("p") ?? "macos") as "macos" | "windows";
const layout = q.get("layout") ?? "default";
const themeStore = createThemeStore({ storageKey: "harness.theme", storage: null, qaOverride: (q.get("t") as "light" | "dark" | null) ?? undefined });
themeStore.apply(themeStore.read());

const dot = (
  <svg viewBox="0 0 16 16" aria-hidden="true"><circle cx="8" cy="8" r="5" fill="currentColor" /></svg>
);
const groups: NavGroup[] = [
  { items: [{ id: "home", label: "Home", icon: dot }, { id: "history", label: "History", icon: dot, badge: "3" }] },
  { title: "Create", items: [{ id: "image", label: "Image", icon: dot }, { id: "video", label: "Video", icon: dot, keywords: ["movie"] }] },
  { title: "Library", items: [{ id: "jobs", label: "Jobs", icon: dot, badge: "2" }, { id: "gallery", label: "Gallery", icon: dot }] },
];
const noop = async () => {};
const win = { minimize: noop, toggleMaximize: noop, close: noop };
const log = (line: string) => {
  const out = document.getElementById("log");
  if (out) out.textContent += `${line}\n`;
};

function Probe() {
  // Wires app behavior the e2e test asserts on.
  const toast = useToast();
  const shell = useShell();
  const [count, setCount] = useState(0);
  useCommands([{ id: "app.hello", label: "Say hello", group: "App", keywords: ["greet"], run: () => { setCount((c) => c + 1); log("command:hello"); } }]);
  useShortcuts([
    { id: "app.save", chord: "mod+s", label: "Save", group: "File", run: () => log("shortcut:save") },
    { id: "app.goto-inbox", chord: "g i", label: "Go to Inbox", group: "Navigation", run: () => log("shortcut:goto-inbox") },
    { id: "app.bare", chord: "x", label: "Bare key action", group: "Navigation", run: () => log("shortcut:bare") },
  ]);
  const rowMenu = useContextMenu<string>((row) => [
    { id: "open", label: `Open ${row}`, run: () => log(`menu:open:${row}`) },
    { id: "copy", label: "Copy", shortcut: "mod+c", run: () => log("menu:copy") },
    { id: "pin", label: "Pinned", checked: true, run: () => log("menu:pin") },
    { id: "del", label: "Delete", danger: true, separatorBefore: true, run: () => log("menu:delete") },
    { id: "off", label: "Unavailable", disabled: true },
  ], "Row actions");
  return (
    <>
      <div id="ran" data-count={count} hidden />
      <div className="rk-card" style={{ marginBottom: 14, display: "flex", gap: 8, flexWrap: "wrap" }}>
        <Button id="b-ok" onClick={() => toast("Saved to library", "success")}>Toast success</Button>
        <Button id="b-err" onClick={() => toast("Engine unreachable", { kind: "error", action: { label: "Retry", run: () => log("toast:retry") } })}>Toast error</Button>
        <Button id="b-short" onClick={() => toast("Quick note", { kind: "info", ttl: 400 })}>Toast short</Button>
        <Button id="b-confirm" variant="primary" onClick={() => void shell.confirm({ title: "Delete 3 items?", description: "This cannot be undone.", confirmLabel: "Delete", danger: true }).then((ok) => log(`confirm:${ok}`))}>Confirm</Button>
        <Button id="b-prompt" onClick={() => void shell.prompt({ title: "Rename", label: "Name", initialValue: "Untitled", validate: (v) => (v.trim() ? null : "Name required") }).then((v) => log(`prompt:${v}`))}>Prompt</Button>
        <Input id="typing" placeholder="type here (bare keys must not fire)" style={{ width: 260 }} />
      </div>
      <div id="row-a" className="rk-card" style={{ marginBottom: 14, display: "flex", alignItems: "center", gap: 10 }} onContextMenu={(e) => rowMenu.open(e, "alpha.mp4")}>
        <strong>alpha.mp4</strong><Badge tone="ok">ready</Badge><span style={{ color: "var(--rk-ink-2)" }}>Right-click for the app-owned menu</span>
      </div>
      {rowMenu.element}
    </>
  );
}

function DefaultLayout() {
  const [active, setActive] = useState("video");
  const [on, setOn] = useState(true);
  const [seg, setSeg] = useState("grid");
  return (
    <AppShell
      groups={groups}
      activeId={active}
      onNavigate={(id) => { setActive(id); log(`nav:${id}`); }}
      title={groups.flatMap((g) => g.items).find((i) => i.id === active)?.label}
      wordmark={<>Acme<span>Right</span></>}
      onOpenSettings={() => log("settings")}
      statusFooter={<><StatusRow tone="ok" label="Engine" value="healthy" /><StatusRow tone="warn" label="Queue" value="1 running" /></>}
      titlebarEnd={<Pill>Queue · idle</Pill>}
      bridge={{ window: win }}
      platform={platform}
      shell={{ themeStore }}
    >
      <PageShell title="Video" subtitle="Generate and review clips." actions={<Button variant="primary" size="sm">New</Button>}>
        <Probe />
        <div style={{ display: "grid", gridTemplateColumns: "repeat(auto-fit,minmax(220px,1fr))", gap: 12 }}>
          <Card><strong>Auto-save</strong><div style={{ marginTop: 8 }}><Toggle checked={on} onChange={setOn} label="Auto-save" /></div></Card>
          <Card><strong>View</strong><div style={{ marginTop: 8 }}><SegmentedControl label="View" value={seg} onChange={setSeg} options={[{ value: "grid", label: "Grid" }, { value: "list", label: "List" }]} /></div></Card>
          <Card><EmptyState title="No clips yet" action={<Button size="sm">Create</Button>}>Generate your first clip.</EmptyState></Card>
        </div>
      </PageShell>
    </AppShell>
  );
}

/** MailRight-style: no fused sidebar, own three-pane layout, own top bar, shared primitives. */
function CustomLayout() {
  const [active, setActive] = useState("home");
  const [menu, setMenu] = useState<{ x: number; y: number } | null>(null);
  const items: MenuItem[] = [{ id: "archive", label: "Archive", shortcut: "e", run: () => log("menu:archive") }, { id: "star", label: "Star", run: () => log("menu:star") }];
  return (
    <AppFrame platform={platform} className="custom">
      <Titlebar platform={platform} windowControls={win} start={<strong style={{ marginLeft: 6 }}>Mail</strong>} title={null} end={<span id="custom-bar" style={{ color: "var(--rk-ink-2)" }}>3 unread</span>} startWidth={150} />
      <div className="rk-body" style={{ gap: 0 }}>
        <div style={{ width: 200, padding: "4px 0" }}><NavList groups={groups.slice(0, 1)} activeId={active} onNavigate={setActive} /></div>
        <Plane>
          <div style={{ display: "grid", gridTemplateColumns: "280px 1fr", flex: 1, minHeight: 0 }}>
            <div style={{ borderRight: ".5px solid var(--rk-hairline)", padding: 8 }} onContextMenu={(e) => { e.preventDefault(); setMenu({ x: e.clientX, y: e.clientY }); }}>
              {["Welcome aboard", "Invoice 2041", "Lunch?"].map((s) => <div key={s} id={`mail-${s[0]}`} style={{ padding: "10px 12px", borderRadius: 8 }}>{s}</div>)}
            </div>
            <div style={{ padding: 24 }}><Probe /></div>
          </div>
        </Plane>
      </div>
      {menu ? <ContextMenu x={menu.x} y={menu.y} items={items} label="Message actions" onClose={() => setMenu(null)} /> : null}
    </AppFrame>
  );
}

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    {layout === "custom" ? (
      <ShellProvider themeStore={themeStore} platform={platform} bridge={{ window: win }}><CustomLayout /></ShellProvider>
    ) : (
      <DefaultLayout />
    )}
    <pre id="log" style={{ position: "fixed", left: -9999, top: 0 }} />
  </StrictMode>,
);
(window as unknown as { __ready: boolean }).__ready = true;
