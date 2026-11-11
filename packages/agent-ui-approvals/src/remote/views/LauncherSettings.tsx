import { useEffect, useState, type ReactNode } from "react";
import { Button, Checkbox, Select, Toggle, Input } from "@rightkit/react-shell/react";
import { formatChord } from "@rightkit/platform-ui";
import { SettingRow, SettingsItem } from "@rightkit/settings-ui/core";
import { useSettingsSearch } from "@rightkit/settings-ui/headless";
import { windowShortcutFrom } from "./settingsKit.js";
import { isWindows } from "@rightkit/system-ui/api";

/** The launcher's user-made items. Stored by the notch as one JSON string. */
export interface LauncherConfig {
  pinnedApps: string[];
  fileFolders: string[];
  appHotkeys: { path: string; hotkey: string }[];
  quicklinks: { name: string; keyword: string; template: string }[];
  snippets: { name: string; keyword: string; body: string }[];
  commands: { name: string; keyword: string; command: string; hotkey: string }[];
  notesEnabled: boolean;
  /** Where "ask" hands a question: the Claude desktop app, or nowhere. */
  /** Short aliases for Apple Shortcuts (Mac). */
  shortcutAliases: ShortcutAlias[];
  /** Global hotkeys that run one Apple Shortcut without opening the launcher (Mac). */
  shortcutHotkeys: ShortcutHotkey[];
}

export interface ShortcutHotkey {
  /** The shortcut's name, as the Shortcuts app shows it. */
  shortcut: string;
  /** Such as "ctrl+opt+s": at least one modifier and a key. */
  hotkey: string;
}

/** The shortcut that already holds `hotkey` besides `shortcut`, or null. */
export function hotkeyTakenBy(list: ShortcutHotkey[], shortcut: string, hotkey: string): string | null {
  if (!hotkey) return null;
  return list.find((h) => h.shortcut !== shortcut && h.hotkey === hotkey)?.shortcut ?? null;
}

export interface ShortcutAlias {
  /** The shortcut's name, as the Shortcuts app shows it. */
  shortcut: string;
  /** 1 to 16 letters or digits, unique without regard to case. */
  alias: string;
}

/** The problem with the alias at `index`, or null when it is usable. */
export function aliasProblem(list: ShortcutAlias[], index: number): string | null {
  const shortcut = list[index].shortcut.trim();
  const alias = list[index].alias.trim();
  if (!shortcut || !alias) return "Enter the shortcut name and an alias.";
  if (!/^[A-Za-z0-9]{1,16}$/.test(alias)) return "The alias is 1 to 16 letters or digits.";
  const taken = list.some((other, j) => j !== index && other.alias.trim().toLowerCase() === alias.toLowerCase());
  return taken ? "Another alias already uses this." : null;
}

const blank = (): LauncherConfig => ({
  pinnedApps: [],
  fileFolders: [],
  appHotkeys: [],
  quicklinks: [],
  snippets: [],
  commands: [],
  notesEnabled: true,
  shortcutAliases: [],
  shortcutHotkeys: [],
});

export function parseLauncherConfig(raw: unknown): LauncherConfig {
  if (typeof raw !== "string" || raw === "") return blank();
  try {
    const parsed = JSON.parse(raw) as Partial<LauncherConfig>;
    return {
      pinnedApps: parsed.pinnedApps ?? [],
      fileFolders: parsed.fileFolders ?? [],
      appHotkeys: parsed.appHotkeys ?? [],
      quicklinks: parsed.quicklinks ?? [],
      snippets: parsed.snippets ?? [],
      commands: parsed.commands ?? [],
      notesEnabled: parsed.notesEnabled ?? true,
      shortcutAliases: parsed.shortcutAliases ?? [],
      shortcutHotkeys: parsed.shortcutHotkeys ?? [],
    };
  } catch {
    return blank();
  }
}

/** A line that adds one value to a list. */
function AddLine({ placeholder, onAdd }: { placeholder: string; onAdd: (value: string) => void }) {
  const [text, setText] = useState("");
  const add = () => {
    const value = text.trim();
    if (!value) return;
    onAdd(value);
    setText("");
  };
  return (
    <div className="ck-line">
      <Input
        className="ck-input"
        aria-label={placeholder}
        value={text}
        placeholder={placeholder}
        onChange={(e) => setText(e.target.value)}
        onKeyDown={(e) => { if (e.key === "Enter") add(); }}
      />
      <Button size="sm" variant="secondary" disabled={!text.trim()} onClick={add}>Add</Button>
    </div>
  );
}

/** Two fields and an Add button for a new shortcut alias; it adds only a usable alias. */
function AliasAdd({ existing, onAdd }: { existing: ShortcutAlias[]; onAdd: (value: ShortcutAlias) => void }) {
  const [shortcut, setShortcut] = useState("");
  const [alias, setAlias] = useState("");
  const candidate = [...existing, { shortcut, alias }];
  const typed = shortcut.trim() !== "" || alias.trim() !== "";
  const problem = typed ? aliasProblem(candidate, candidate.length - 1) : null;
  const add = () => {
    if (problem !== null || !typed) return;
    onAdd({ shortcut: shortcut.trim(), alias: alias.trim() });
    setShortcut("");
    setAlias("");
  };
  return (
    <>
      <div className="ck-line">
        <Input className="ck-input" aria-label="Shortcut name" placeholder="Toggle Speakers"
          value={shortcut} style={{ flex: "2 1 220px" }} onChange={(e) => setShortcut(e.target.value)} />
        <Input className="ck-input" aria-label="Alias" placeholder="ts"
          value={alias} style={{ flex: "1 1 110px" }} onChange={(e) => setAlias(e.target.value)}
          onKeyDown={(e) => { if (e.key === "Enter") add(); }} />
        <Button size="sm" variant="secondary" disabled={problem !== null || !typed} onClick={add}>Add</Button>
      </div>
      {problem ? <div className="ck-sub" role="status">{problem}</div> : null}
    </>
  );
}

/** "ctrl+opt+s" as "Control Option S". */
function hotkeyText(raw: string): string {
  const names: Record<string, string> = { ctrl: "Control", opt: "Option", shift: "Shift", cmd: "Command" };
  return raw.split("+").map((p) => names[p] ?? (p.length === 1 ? p.toUpperCase() : p[0].toUpperCase() + p.slice(1))).join(" ");
}

/** One Apple Shortcut: its name, its hotkey, a recorder to set it and a button to clear it. */
function ShortcutHotkeyLine({ name, hotkey, takenBy, onChange }: {
  name: string;
  hotkey: string;
  takenBy: string | null;
  onChange: (hotkey: string) => void;
}) {
  const [recording, setRecording] = useState(false);
  useEffect(() => {
    if (!recording) return;
    const onKey = (e: KeyboardEvent) => {
      e.preventDefault();
      e.stopPropagation();
      if (e.code === "Escape") {
        setRecording(false);
        return;
      }
      const shortcut = windowShortcutFrom(e);
      if (!shortcut) return;
      onChange(shortcut);
      setRecording(false);
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [recording, onChange]);
  return (
    <div className="ck-list">
      <div className="ck-line">
        <span className="ck-sub" style={{ flex: "2 1 220px" }}>{name}</span>
        <span className="ck-sub ck-num ck-keys" style={{ flex: "1 1 110px" }}>
          {recording ? "Press a shortcut…" : hotkey ? hotkeyText(hotkey) : "None"}
        </span>
        <Button size="sm" variant="secondary" onClick={() => setRecording(!recording)}>{recording ? "Cancel" : "Set"}</Button>
        <Button size="sm" variant="ghost" disabled={!hotkey} onClick={() => onChange("")}>Clear</Button>
      </div>
      {takenBy ? <div className="ck-sub" role="status">{takenBy} already uses this hotkey.</div> : null}
    </div>
  );
}

function Section({ title, note, children }: { title: string; note?: string; children: ReactNode }) {
  const { highlight } = useSettingsSearch();
  return (
    <SettingsItem className="ck-set ck-stack" text={`${title} ${note ?? ""}`}>
      <div className="ck-text">
        <strong>{highlight(title)}</strong>
        {note && <div className="ck-sub">{highlight(note)}</div>}
      </div>
      <div className="ck-list">{children}</div>
    </SettingsItem>
  );
}

/**
 * Edits the launcher's lists: folders for file search, pinned apps, app and
 * command hotkeys, quicklinks, snippets and custom commands. Changes are held
 * until Save, then sent as one `launcherConfig` value.
 */
const ENGINES: { value: string; label: string }[] = [
  { value: "google", label: "Google" },
  { value: "bing", label: "Bing" },
  { value: "duckduckgo", label: "DuckDuckGo" },
];

/** A launcher chord's name from its published value: "optionShiftSpace" -> "Option+Shift+Space",
 *  "ctrlSpace" -> "Ctrl+Space". Covers the Mac's stand-down chords without a list to keep in step. */
export const chordName = (raw: string) =>
  formatChord(raw.replace(/([a-z])([A-Z])/g, "$1+$2"), "unknown").join("+");

/** The launcher's typed-text rows and the Alt+Space answer: shared keys, shown on both platforms.
 *  `s` is the notch's published settings; a row shows only when its key is published. */
export function LauncherSearchRows({ s, set }: {
  s: Record<string, string | number | boolean>;
  set: (key: string, value: unknown) => void;
}) {
  const has = (key: string) => key in s;
  const webOn = s.launcherWebSearch !== false;
  const conflict = String(s.launcherHotkeyConflict ?? "");
  const alt = conflict === "pulse" ? "Pulse" : conflict === "other" ? "Kept for the other app" : "Not asked yet";
  const chord = isWindows ? "Alt+Space" : "Option+Space";
  return (
    <>
      {has("launcherWebSearch") ? (
        <SettingRow className="ck-set" label="Web search row" description="A closing row that searches the web for what you typed, in your default browser. On by default.">
          <Toggle checked={webOn} onChange={(v) => set("launcherWebSearch", v)} label="Web search row" />
        </SettingRow>
      ) : null}
      {has("launcherWebSearchEngine") ? (
        <SettingRow className="ck-set" label="Search engine" description="Which engine the web search row uses. Google by default.">
          <Select label="Search engine" disabled={!webOn}
            value={String(s.launcherWebSearchEngine || "google")}
            onChange={(v) => set("launcherWebSearchEngine", v)}
            options={ENGINES} />
        </SettingRow>
      ) : null}
      {has("launcherHotkeyConflict") ? (
        <SettingRow className="ck-set" label={chord} description="Who gets the chord when another program already holds it. Pulse asks once.">
          <span className="ck-sub" role="status">{alt}</span>
          <Button size="sm" variant="secondary" disabled={conflict === ""}
            onClick={() => set("launcherHotkeyConflict", "")}>Ask again</Button>
        </SettingRow>
      ) : null}
    </>
  );
}

export function LauncherLists({ raw, shortcutNames = [], onSave }: {
  raw: unknown;
  /** The Apple Shortcuts the notch found (`launcherShortcutNames`), Mac only. */
  shortcutNames?: string[];
  onSave: (next: LauncherConfig) => void;
}) {
  const key = typeof raw === "string" ? raw : "";
  const [draft, setDraft] = useState<LauncherConfig>(() => parseLauncherConfig(key));
  const [dirty, setDirty] = useState(false);

  // The notch's copy changed (or our save landed): show it.
  useEffect(() => {
    setDraft(parseLauncherConfig(key));
    setDirty(false);
  }, [key]);

  const edit = (next: LauncherConfig) => {
    setDraft(next);
    setDirty(true);
  };
  const input = (value: string, placeholder: string, onChange: (value: string) => void, flex = 1) => (
    <Input
      className="ck-input"
      aria-label={placeholder}
      value={value}
      placeholder={placeholder}
      style={{ flex: `${flex} 1 ${flex * 110}px` }}
      onChange={(e) => onChange(e.target.value)}
    />
  );
  const removeAt = <T,>(list: T[], index: number) => list.filter((_, i) => i !== index);
  const hotkeyNote = "Needs a modifier: for example opt+1, cmd+shift+k or ctrl+opt+d.";

  return (
    <>
      {(
      <Section title="Folders for file search" note={isWindows ? "The launcher searches file names only inside these folders. Empty turns file search off." : "Pulse searches file names itself, with no Spotlight index, only inside these folders. Empty turns file search off. Command space stays taken until Spotlight's shortcut is turned off in System Settings."}>
        {draft.fileFolders.map((path, i) => (
          <div key={`folder-${i}`} className="ck-line">
            <span className="ck-sub ck-path">{path}</span>
            <Button size="sm" variant="ghost" onClick={() => edit({ ...draft, fileFolders: removeAt(draft.fileFolders, i) })}>Remove</Button>
          </div>
        ))}
        <AddLine placeholder={isWindows ? "C:\\Users\\you\\Documents" : "/Users/you/Documents"} onAdd={(v) => edit({ ...draft, fileFolders: [...draft.fileFolders, v] })} />
      </Section>
      )}

      <Section title="Pinned apps" note={isWindows ? "Pin an app in the launcher with Alt+P. Pinned apps show first when nothing is typed." : "Pin an app in the launcher with Command-P. Pinned apps show first when nothing is typed."}>
        {draft.pinnedApps.map((path, i) => (
          <div key={`pin-${i}`} className="ck-line">
            <span className="ck-sub ck-path">{path}</span>
            <Button size="sm" variant="ghost" onClick={() => edit({ ...draft, pinnedApps: removeAt(draft.pinnedApps, i) })}>Remove</Button>
          </div>
        ))}
        <AddLine placeholder={isWindows ? "C:\\Program Files\\App\\app.exe" : "/Applications/Safari.app"} onAdd={(v) => edit({ ...draft, pinnedApps: [...draft.pinnedApps, v] })} />
      </Section>

      {!isWindows ? (
      <Section title="App hotkeys" note={`Press to show the app, or hide it when it is in front. ${hotkeyNote}`}>
        {draft.appHotkeys.map((item, i) => (
          <div key={`app-hotkey-${i}`} className="ck-line">
            {input(item.path, "/Applications/Safari.app", (v) => {
              const next = [...draft.appHotkeys];
              next[i] = { ...item, path: v };
              edit({ ...draft, appHotkeys: next });
            }, 2)}
            {input(item.hotkey, "opt+1", (v) => {
              const next = [...draft.appHotkeys];
              next[i] = { ...item, hotkey: v };
              edit({ ...draft, appHotkeys: next });
            })}
            <Button size="sm" variant="ghost" onClick={() => edit({ ...draft, appHotkeys: removeAt(draft.appHotkeys, i) })}>Remove</Button>
          </div>
        ))}
        <Button size="sm" variant="secondary"
          onClick={() => edit({ ...draft, appHotkeys: [...draft.appHotkeys, { path: "", hotkey: "" }] })}>Add an app hotkey</Button>
      </Section>
      ) : null}

      <Section title="Quicklinks" note="{query} is replaced by the text after the keyword. {clipboard} and {date} are also filled in.">
        {draft.quicklinks.map((item, i) => (
          <div key={`quicklink-${i}`} className="ck-line">
            {input(item.name, "Name", (v) => {
              const next = [...draft.quicklinks];
              next[i] = { ...item, name: v };
              edit({ ...draft, quicklinks: next });
            }, 1)}
            {input(item.keyword, "Keyword", (v) => {
              const next = [...draft.quicklinks];
              next[i] = { ...item, keyword: v };
              edit({ ...draft, quicklinks: next });
            }, 1)}
            {input(item.template, "https://github.com/search?q={query}", (v) => {
              const next = [...draft.quicklinks];
              next[i] = { ...item, template: v };
              edit({ ...draft, quicklinks: next });
            }, 3)}
            <Button size="sm" variant="ghost" onClick={() => edit({ ...draft, quicklinks: removeAt(draft.quicklinks, i) })}>Remove</Button>
          </div>
        ))}
        <Button size="sm" variant="secondary"
          onClick={() => edit({ ...draft, quicklinks: [...draft.quicklinks, { name: "", keyword: "", template: "" }] })}>Add a quicklink</Button>
      </Section>

      <Section title="Snippets" note="Markdown text pasted into the app you were using. {date}, {clipboard} and {argument} are filled in. {snippet:Name} includes another snippet by its name or keyword; a snippet that includes itself is left as written. To expand a snippet wherever you type, give it a keyword that starts with a symbol, such as ;sig, and turn on “Expand snippet keywords as you type” above.">
        {draft.snippets.map((item, i) => (
          <div key={`snippet-${i}`} className="ck-list">
            <div className="ck-line">
              {input(item.name, "Name", (v) => {
                const next = [...draft.snippets];
                next[i] = { ...item, name: v };
                edit({ ...draft, snippets: next });
              })}
              {input(item.keyword, "Keyword", (v) => {
                const next = [...draft.snippets];
                next[i] = { ...item, keyword: v };
                edit({ ...draft, snippets: next });
              })}
              <Button size="sm" variant="ghost" onClick={() => edit({ ...draft, snippets: removeAt(draft.snippets, i) })}>Remove</Button>
            </div>
            <textarea
              className="ck-input"
              aria-label="Snippet text"
              rows={4}
              value={item.body}
              placeholder={"Thanks,\n{clipboard}"}
              onChange={(e) => {
                const next = [...draft.snippets];
                next[i] = { ...item, body: e.target.value };
                edit({ ...draft, snippets: next });
              }}
            />
          </div>
        ))}
        <Button size="sm" variant="secondary"
          onClick={() => edit({ ...draft, snippets: [...draft.snippets, { name: "", keyword: "", body: "" }] })}>Add a snippet</Button>
      </Section>

      {!isWindows ? (
      <Section title="Shortcut aliases" note="Type an alias in the launcher to put that Apple Shortcut first. Use the shortcut's exact name from the Shortcuts app. An alias is 1 to 16 letters or digits, and each is unique (case does not count).">
        {draft.shortcutAliases.map((item, i) => {
          const problem = aliasProblem(draft.shortcutAliases, i);
          return (
            <div key={`alias-${i}`} className="ck-list">
              <div className="ck-line">
                {input(item.shortcut, "Shortcut name", (v) => {
                  const next = [...draft.shortcutAliases];
                  next[i] = { ...item, shortcut: v };
                  edit({ ...draft, shortcutAliases: next });
                }, 2)}
                {input(item.alias, "Alias", (v) => {
                  const next = [...draft.shortcutAliases];
                  next[i] = { ...item, alias: v };
                  edit({ ...draft, shortcutAliases: next });
                }, 1)}
                <Button size="sm" variant="ghost" onClick={() => edit({ ...draft, shortcutAliases: removeAt(draft.shortcutAliases, i) })}>Remove</Button>
              </div>
              {problem ? <div className="ck-sub" role="status">{problem}</div> : null}
            </div>
          );
        })}
        <AliasAdd existing={draft.shortcutAliases}
          onAdd={(value) => edit({ ...draft, shortcutAliases: [...draft.shortcutAliases, value] })} />
      </Section>
      ) : null}

      {!isWindows ? (
      <Section title="Shortcut hotkeys" note="A global hotkey that runs one Apple Shortcut without opening the launcher. Needs at least one of Control, Option, Shift or Command; Escape cancels. Press Set, then the keys. Save launcher lists to apply. None by default.">
        {(() => {
          const names = [...shortcutNames];
          draft.shortcutHotkeys.forEach((h) => { if (!names.includes(h.shortcut)) names.push(h.shortcut); });
          if (names.length === 0) {
            return <div className="ck-sub" role="status">No shortcuts found. They are listed while Apple Shortcuts is on and this window is open.</div>;
          }
          return names.map((name) => {
            const hotkey = draft.shortcutHotkeys.find((h) => h.shortcut === name)?.hotkey ?? "";
            return (
              <ShortcutHotkeyLine key={`shortcut-hotkey-${name}`} name={name} hotkey={hotkey}
                takenBy={hotkeyTakenBy(draft.shortcutHotkeys, name, hotkey)}
                onChange={(next) => edit({
                  ...draft,
                  shortcutHotkeys: [
                    ...draft.shortcutHotkeys.filter((h) => h.shortcut !== name),
                    ...(next ? [{ shortcut: name, hotkey: next }] : []),
                  ],
                })} />
            );
          });
        })()}
      </Section>
      ) : null}

      {isWindows ? (
      <Section title="Notes" note="Type “notes” in the launcher to list or search your notes, “note text” to start one with that text, and “notes trash” for deleted ones, which are kept for 30 days. Each note is a Markdown file on this PC and opens in the program Windows uses for it; Alt+D deletes the selected note.">
        <Checkbox className="ck-line" checked={draft.notesEnabled}
          onChange={(on) => edit({ ...draft, notesEnabled: on })} label="Notes in the launcher" />
      </Section>
      ) : (
      <Section title="Notes" note="Type “notes” in the launcher to list, search or start a note; “note text” starts one with that text. Notes are kept on this Mac only, and deleted ones can be restored for 30 days with “notes trash”.">
        <Checkbox className="ck-line" checked={draft.notesEnabled}
          onChange={(on) => edit({ ...draft, notesEnabled: on })} label="Notes in the launcher" />
      </Section>
      )}

      {(
      <Section title="Custom commands" note={isWindows ? "Run with cmd.exe /C; the output shows in the launcher. A command is stopped after 30 seconds." : "Run with /bin/zsh -lc; the output shows in the launcher. Optional hotkey runs it from anywhere."}>
        {draft.commands.map((item, i) => (
          <div key={`command-${i}`} className="ck-line">
            {input(item.name, "Name", (v) => {
              const next = [...draft.commands];
              next[i] = { ...item, name: v };
              edit({ ...draft, commands: next });
            })}
            {input(item.keyword, "Keyword", (v) => {
              const next = [...draft.commands];
              next[i] = { ...item, keyword: v };
              edit({ ...draft, commands: next });
            })}
            {input(item.command, isWindows ? "ipconfig" : "df -h /", (v) => {
              const next = [...draft.commands];
              next[i] = { ...item, command: v };
              edit({ ...draft, commands: next });
            }, 2)}
            {isWindows ? null : input(item.hotkey, "hotkey", (v) => {
              const next = [...draft.commands];
              next[i] = { ...item, hotkey: v };
              edit({ ...draft, commands: next });
            })}
            <Button size="sm" variant="ghost" onClick={() => edit({ ...draft, commands: removeAt(draft.commands, i) })}>Remove</Button>
          </div>
        ))}
        <Button size="sm" variant="secondary"
          onClick={() => edit({ ...draft, commands: [...draft.commands, { name: "", keyword: "", command: "", hotkey: "" }] })}>Add a command</Button>
      </Section>
      )}

      <div className="ck-set ck-savebar">
        <span className="ck-sub" role="status">{dirty ? "Not saved yet." : "Launcher lists are saved."}</span>
        <Button size="sm" variant="secondary"
          disabled={!dirty
            || draft.shortcutAliases.some((_, i) => aliasProblem(draft.shortcutAliases, i) !== null)
            || draft.shortcutHotkeys.some((h) => hotkeyTakenBy(draft.shortcutHotkeys, h.shortcut, h.hotkey) !== null)}
          onClick={() => onSave({
            ...draft,
            shortcutAliases: draft.shortcutAliases.map((a) => ({ shortcut: a.shortcut.trim(), alias: a.alias.trim() })),
          })}>Save launcher lists</Button>
      </div>
    </>
  );
}
