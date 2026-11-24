import type { Platform } from "./format.js";

/**
 * Push-to-talk key matching for a webview that previews the hold-to-talk key. `code` is a
 * `KeyboardEvent.code`; `configuredKey` is the setting's key alias (`lalt`, `ralt`, `rctrl`,
 * the aliases of `rightkit-hotkeys` `PttAlias`). The two functions differ by platform because the
 * native hook treats an unknown alias differently: Windows falls back to right Alt, macOS to either Control.
 */
export function isWindowsPttKeyEvent(code: string, configuredKey: string): boolean {
  if (configuredKey === "lalt") return code === "AltLeft";
  return code === (configuredKey === "rctrl" ? "ControlRight" : "AltRight");
}

export function isMacPttKeyEvent(code: string, configuredKey: string): boolean {
  if (configuredKey === "lalt") return code === "AltLeft";
  if (configuredKey === "ralt") return code === "AltRight";
  return code === "ControlLeft" || code === "ControlRight";
}

/**
 * The key chord token (for `formatChord`) of the push-to-talk key on a platform. `alias` is the
 * configured key (`lalt`, `ralt`, `rctrl`, the aliases of `rightkit-hotkeys` `PttAlias`). On macOS
 * an alias other than `lalt` or `ralt` means either Control (`"control"`); on iOS the in-app
 * control (`"record"`); elsewhere an unknown alias falls back to right Alt, as the native hook does.
 * Default alias `ralt`.
 */
export function pushToTalkChord(os: Platform, alias: string = "ralt"): string {
  if (os === "macos") return alias === "lalt" || alias === "ralt" ? alias : "control";
  if (os === "ios") return "record";
  return alias === "rctrl" ? "rctrl" : alias === "lalt" ? "lalt" : "ralt";
}

/**
 * Words for the push-to-talk help text. Every field is optional; the default is the English shown.
 * The host passes its own wording (or a translation) for any it changes.
 */
export interface PushToTalkLabels {
  /** Key names by alias on macOS. Defaults `Left Option`, `Right Option`; any other alias reads `Control`. */
  macKeys?: { lalt?: string; ralt?: string; control?: string };
  /** Key names by alias elsewhere. Defaults `Left Alt`, `Right Alt`, `Right Ctrl`. */
  keys?: { lalt?: string; ralt?: string; rctrl?: string };
  /** The macOS modifier that turns a release into paste + send. Default `fn`. */
  macSendKey?: string;
  /** Sentence naming the key to hold. Default `Hold ${key}, speak, release to paste.` */
  hold?: (key: string) => string;
  /** Sentence on macOS about the send modifier. Default `Press ${sendKey} before releasing to paste + send.` */
  macSend?: (sendKey: string) => string;
  /** Sentence elsewhere naming the extra key. Default `Add ${key} to paste + send.` */
  send?: (key: string) => string;
  /** Text on iOS, where there is no key. Default `Use the in-app record control.` */
  ios?: string;
  /** Gesture on macOS. Default `Press ${sendKey}, then release push-to-talk`. */
  macSendGesture?: (sendKey: string) => string;
  /** Gesture elsewhere. Default `Add ${key}, then release push-to-talk`. */
  sendGesture?: (key: string) => string;
}

function pttNames(labels: PushToTalkLabels) {
  return {
    mac: { lalt: "Left Option", ralt: "Right Option", control: "Control", ...labels.macKeys },
    win: { lalt: "Left Alt", ralt: "Right Alt", rctrl: "Right Ctrl", ...labels.keys },
    macSendKey: labels.macSendKey ?? "fn",
  };
}

/** The key that, added to the push-to-talk key on Windows and Linux, sends: Right Alt with `rctrl`, else Right Ctrl. */
function nonMacSendKey(alias: string, win: { ralt: string; rctrl: string }): string {
  return alias === "rctrl" ? win.ralt : win.rctrl;
}

/**
 * One line of help for the push-to-talk key: which key to hold and how to send. The host passes the
 * platform (`getPlatform()`), the configured key alias and, if it changes the wording, `labels`.
 * Default alias `ralt`.
 */
export function pushToTalkCopy(os: Platform, alias: string = "ralt", labels: PushToTalkLabels = {}): string {
  const names = pttNames(labels);
  const hold = labels.hold ?? ((key: string) => `Hold ${key}, speak, release to paste.`);
  if (os === "macos") {
    const key = alias === "lalt" ? names.mac.lalt : alias === "ralt" ? names.mac.ralt : names.mac.control;
    const send = labels.macSend ?? ((sendKey: string) => `Press ${sendKey} before releasing to paste + send.`);
    return `${hold(key)} ${send(names.macSendKey)}`;
  }
  if (os === "ios") return labels.ios ?? "Use the in-app record control.";
  const key = alias === "rctrl" ? names.win.rctrl : alias === "lalt" ? names.win.lalt : names.win.ralt;
  const send = labels.send ?? ((extra: string) => `Add ${extra} to paste + send.`);
  return `${hold(key)} ${send(nonMacSendKey(alias, names.win))}`;
}

/**
 * The short gesture that sends instead of only pasting, beside the push-to-talk key (for a controls
 * table): `Press fn, then release push-to-talk` on macOS, `Add Right Ctrl, then release push-to-talk`
 * elsewhere (Right Alt when the configured key is `rctrl`). Same inputs as {@link pushToTalkCopy}.
 */
export function pushToTalkSendGesture(os: Platform, alias: string = "ralt", labels: PushToTalkLabels = {}): string {
  const names = pttNames(labels);
  if (os === "macos") {
    return (labels.macSendGesture ?? ((sendKey: string) => `Press ${sendKey}, then release push-to-talk`))(names.macSendKey);
  }
  const gesture = labels.sendGesture ?? ((key: string) => `Add ${key}, then release push-to-talk`);
  return gesture(nonMacSendKey(alias, names.win));
}
