// macOS native menu bridge. The document's File/View/Format/Insert commands live in the system
// menu bar on Mac instead of in-window menus. Rust builds the menu once (every item disabled);
// this hook tells it which `vr-cmd:` items apply to the active document (and which are checked),
// and runs the matching command when the menu fires `viewright:menu-command`.
import { invoke } from "@tauri-apps/api/core";
import { useEffect, useRef } from "react";
import { isMac } from "../../platform";
import { inTauri } from "../../state/tauriBridge";

export interface NativeMenuCommand {
  id: string;
  run: () => void;
  checked?: boolean;
}

export const MENU_COMMAND_EVENT = "viewright:menu-command";

/** Menu-bar formatting commands for the active Markdown editor (event detail = command id).
 *  Lives here (not in documents/markdown/) because the dispatcher is format-agnostic app chrome —
 *  keeping it in the markdown tree would pull a markdown module into every format's closure. */
export const MARKDOWN_FORMAT_EVENT = "viewright:markdown-format";

export function nativeMenuActive(): boolean {
  return isMac() && inTauri();
}

export function useNativeDocumentMenu(commands: NativeMenuCommand[] | null): void {
  const latest = useRef(commands);
  latest.current = commands;

  useEffect(() => {
    if (!commands) return;
    const onCommand = (event: Event) => {
      const id = (event as CustomEvent<string>).detail;
      latest.current?.find((command) => command.id === id)?.run();
    };
    window.addEventListener(MENU_COMMAND_EVENT, onCommand);
    return () => window.removeEventListener(MENU_COMMAND_EVENT, onCommand);
  }, [commands]);

  const enabled = commands?.map((command) => command.id) ?? [];
  const checked = commands?.filter((command) => command.checked).map((command) => command.id) ?? [];
  const stateKey = `${enabled.join(",")}|${checked.join(",")}`;
  useEffect(() => {
    if (!nativeMenuActive()) return;
    const [on, ticked] = stateKey.split("|").map((part) => (part ? part.split(",") : []));
    void invoke("set_native_menu_state", { enabled: on, checked: ticked }).catch(() => {});
  }, [stateKey]);

  // Leaving a document (or the app shell unmounting) greys every document command out again.
  useEffect(
    () => () => {
      if (nativeMenuActive())
        void invoke("set_native_menu_state", { enabled: [], checked: [] }).catch(() => {});
    },
    [],
  );
}
