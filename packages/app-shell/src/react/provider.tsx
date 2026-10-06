import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useState,
  useSyncExternalStore,
  type ReactNode,
} from "react";
import { getPlatform, type Platform } from "@rightkit/platform-ui";
import { createCommandRegistry, type Command, type CommandRegistry } from "../commands.js";
import type { AppShellBridge } from "../bridge.js";
import { createShortcutRegistry, type ShortcutDef, type ShortcutRegistry } from "../shortcuts.js";
import { createThemeStore, resolveTheme, type ResolvedTheme, type ThemeChoice, type ThemeStore } from "../theme.js";
import { createToastBus, type ToastBus, type ToastOptions } from "../toast.js";
import { useLatest, useMediaQuery } from "./hooks.js";
import { ConfirmDialog, PromptDialog, type ConfirmDialogProps, type PromptDialogProps } from "./overlays.js";
import { CommandPalette, ShortcutHelp, type PaletteLabels } from "./palette.js";
import { ToastViewport } from "./toasts.js";

type ConfirmOptions = Omit<ConfirmDialogProps, "onConfirm" | "onCancel">;
type PromptOptions = Omit<PromptDialogProps, "onSubmit" | "onCancel">;

type ModalRequest =
  | { kind: "confirm"; options: ConfirmOptions; resolve: (value: boolean) => void }
  | { kind: "prompt"; options: PromptOptions; resolve: (value: string | null) => void };

export interface ShellContextValue {
  platform: Platform;
  registry: ShortcutRegistry;
  commands: CommandRegistry;
  toasts: ToastBus;
  bridge: AppShellBridge | undefined;
  themeStore: ThemeStore;
  palette: { open: boolean; show: () => void; hide: () => void; toggle: () => void };
  help: { open: boolean; show: () => void; hide: () => void; toggle: () => void };
  /** Promise-based dialogs; resolve false / null on Escape or backdrop. */
  confirm: (options: ConfirmOptions) => Promise<boolean>;
  prompt: (options: PromptOptions) => Promise<string | null>;
}

const ShellContext = createContext<ShellContextValue | null>(null);

export interface ShellProviderProps {
  children: ReactNode;
  /** Theme store from `createThemeStore`; default persists under `rightkit.theme`. */
  themeStore?: ThemeStore;
  /** Inject a registry/bus (tests, shared across windows). */
  registry?: ShortcutRegistry;
  toastBus?: ToastBus;
  bridge?: AppShellBridge;
  platform?: Platform;
  /** Palette chord. `false` disables the built-in binding. Default "mod+k". */
  paletteChord?: string | false;
  /** Shortcut-help chord. `false` disables. Default "?". */
  helpChord?: string | false;
  paletteLabels?: Partial<PaletteLabels>;
  helpTitle?: string;
  /** Render palette/help/dialogs/toasts. Default true; set false to mount them yourself. */
  mountOverlays?: boolean;
}

export function ShellProvider({
  children,
  themeStore,
  registry: registryProp,
  toastBus,
  bridge,
  platform: platformProp,
  paletteChord = "mod+k",
  helpChord = "?",
  paletteLabels,
  helpTitle,
  mountOverlays = true,
}: ShellProviderProps) {
  const platform = platformProp ?? getPlatform();
  const registry = useMemo(() => registryProp ?? createShortcutRegistry({ platform }), [registryProp, platform]);
  const commands = useMemo(() => createCommandRegistry(), []);
  const bus = useMemo(() => toastBus ?? createToastBus(), [toastBus]);
  const store = useMemo(() => themeStore ?? createThemeStore({ storageKey: "rightkit.theme" }), [themeStore]);
  const [paletteOpen, setPaletteOpen] = useState(false);
  const [helpOpen, setHelpOpen] = useState(false);
  const [modals, setModals] = useState<ModalRequest[]>([]);
  const bridgeRef = useLatest(bridge);

  const palette = useMemo(
    () => ({ open: paletteOpen, show: () => setPaletteOpen(true), hide: () => setPaletteOpen(false), toggle: () => setPaletteOpen((o) => !o) }),
    [paletteOpen],
  );
  const help = useMemo(
    () => ({ open: helpOpen, show: () => setHelpOpen(true), hide: () => setHelpOpen(false), toggle: () => setHelpOpen((o) => !o) }),
    [helpOpen],
  );

  const confirm = useCallback((options: ConfirmOptions) => new Promise<boolean>((resolve) => setModals((m) => [...m, { kind: "confirm", options, resolve }])), []);
  const prompt = useCallback((options: PromptOptions) => new Promise<string | null>((resolve) => setModals((m) => [...m, { kind: "prompt", options, resolve }])), []);

  // Apply the stored theme so `data-theme` is right for children.
  useEffect(() => store.apply(store.read()), [store]);

  // Window key dispatch.
  useEffect(() => registry.attach(window), [registry]);

  // Built-in bindings. Apps override by registering the same id, or disable via props.
  useEffect(() => {
    const offs: Array<() => void> = [];
    if (paletteChord) {
      offs.push(registry.register({ id: "shell.palette", chord: paletteChord, label: "Command palette", group: "General", allowInInput: true, run: () => setPaletteOpen((o) => !o) }));
    }
    if (helpChord) {
      offs.push(
        registry.register({
          id: "shell.help",
          chord: helpChord,
          label: "Keyboard shortcuts",
          group: "General",
          run: () => setHelpOpen((o) => !o),
        }),
      );
    }
    return () => offs.forEach((off) => off());
  }, [registry, paletteChord, helpChord]);

  // Native menu / tray / deep-link actions route to a command or shortcut with the same id.
  const onMenuAction = bridge?.onMenuAction;
  useEffect(() => {
    if (!onMenuAction) return;
    let off: (() => void) | undefined;
    let cancelled = false;
    void Promise.resolve(
      onMenuAction((id) => {
        if (!commands.run(id)) registry.invoke(id);
      }),
    ).then((fn) => {
      if (cancelled) fn?.();
      else off = fn;
    });
    return () => {
      cancelled = true;
      off?.();
    };
  }, [onMenuAction, commands, registry]);

  // Mirror theme to native chrome.
  const choice = useSyncExternalStore(store.subscribe, store.read, store.read);
  useEffect(() => {
    void bridgeRef.current?.setNativeTheme?.(choice);
  }, [choice, bridgeRef]);

  const value = useMemo<ShellContextValue>(
    () => ({ platform, registry, commands, toasts: bus, bridge, themeStore: store, palette, help, confirm, prompt }),
    [platform, registry, commands, bus, bridge, store, palette, help, confirm, prompt],
  );

  const paletteCommands = useSyncExternalStore(commands.subscribe, commands.list, commands.list);
  const top = modals[modals.length - 1];
  const settle = (resolveWith: () => void) => {
    resolveWith();
    setModals((m) => m.slice(0, -1));
  };

  return (
    <ShellContext.Provider value={value}>
      {children}
      {mountOverlays ? (
        <>
          {paletteOpen ? <CommandPalette commands={paletteCommands} registry={registry} labels={paletteLabels} onClose={palette.hide} /> : null}
          {helpOpen ? <ShortcutHelp registry={registry} title={helpTitle} onClose={help.hide} /> : null}
          {top?.kind === "confirm" ? (
            <ConfirmDialog {...top.options} onConfirm={() => settle(() => top.resolve(true))} onCancel={() => settle(() => top.resolve(false))} />
          ) : null}
          {top?.kind === "prompt" ? (
            <PromptDialog {...top.options} onSubmit={(v) => settle(() => top.resolve(v))} onCancel={() => settle(() => top.resolve(null))} />
          ) : null}
          <ToastViewport bus={bus} />
        </>
      ) : null}
    </ShellContext.Provider>
  );
}

export function useShellOptional(): ShellContextValue | null {
  return useContext(ShellContext);
}

export function useShell(): ShellContextValue {
  const ctx = useContext(ShellContext);
  if (!ctx) throw new Error("useShell must be used inside <ShellProvider> (or <AppShell>).");
  return ctx;
}

/** Register palette commands for the lifetime of the calling component. */
export function useCommands(commands: readonly Command[]): void {
  const { commands: registry } = useShell();
  const latest = useLatest(commands);
  // Re-register when the visible set changes; `run` closures stay current through `latest`.
  const signature = commands.map((c) => [c.id, c.label, c.hint ?? "", c.group ?? "", c.disabled ? 1 : 0, c.shortcut ?? "", c.chord ?? ""].join("\u0000")).join("\u0001");
  useEffect(() => {
    const wrapped = (latest.current ?? []).map((c) => ({ ...c, run: () => (latest.current ?? []).find((x) => x.id === c.id)?.run() }));
    return registry.register(wrapped);
  }, [registry, signature, latest]);
}

/** Register shortcuts for the lifetime of the calling component. */
export function useShortcuts(defs: readonly ShortcutDef[]): void {
  const { registry } = useShell();
  const latest = useLatest(defs);
  const signature = defs.map((d) => [d.id, typeof d.chord === "string" ? d.chord : d.chord.join("|"), d.label, d.group ?? ""].join("\u0000")).join("\u0001");
  useEffect(() => {
    const find = (id: string) => (latest.current ?? []).find((x) => x.id === id);
    const offs = (latest.current ?? []).map((d) =>
      registry.register({ ...d, run: (e) => find(d.id)?.run(e), when: d.when ? () => find(d.id)?.when?.() ?? true : undefined }),
    );
    return () => offs.forEach((off) => off());
  }, [registry, signature, latest]);
}

/** `const toast = useToast(); toast("Saved", "success")`. */
export function useToast(): (message: string, opts?: ToastOptions | "info" | "success" | "warning" | "error") => number {
  const { toasts } = useShell();
  return useCallback((message, opts) => toasts.push(message, opts), [toasts]);
}

/** Theme choice + resolved light/dark, persisted through the provider's store. */
export function useTheme(): { choice: ThemeChoice; resolved: ResolvedTheme; setChoice: (choice: ThemeChoice) => void } {
  const { themeStore } = useShell();
  const choice = useSyncExternalStore(themeStore.subscribe, themeStore.read, themeStore.read);
  const dark = useMediaQuery("(prefers-color-scheme: dark)");
  const setChoice = useCallback((next: ThemeChoice) => themeStore.save(next), [themeStore]);
  return { choice, resolved: resolveTheme(choice, dark), setChoice };
}
