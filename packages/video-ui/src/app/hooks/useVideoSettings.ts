import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { videoAppHost } from "../../host.js";
import { engineCall, errorMessage } from "../../lib/engine.js";
import { storageClear, storageStatus } from "../../lib/media.js";
import { pickFile, pickFolder } from "../lib/dialog.js";

/** Stable host-supplied controllers and application version. */
export function useVideoSettings<Backend, Models>(createBackend: () => Backend, createModels: () => Models) {
  const backend = useMemo(createBackend, [createBackend]);
  const models = useMemo(createModels, [createModels]);
  const [version, setVersion] = useState("");
  useEffect(() => {
    let live = true;
    Promise.resolve().then(() => videoAppHost().appVersion())
      .then((value) => { if (live) setVersion(value); })
      .catch(() => { if (live) setVersion(""); });
    return () => { live = false; };
  }, []);
  return { backend, models, version };
}

type SettingsSet = (key: string, value: unknown) => Promise<void>;

export function videoEditingSettings(values: Record<string, unknown>, set: SettingsSet) {
  const formats = (values.default_formats as string[] | undefined) ?? ["16:9"];
  return {
    formats,
    toggleFormat: (format: string) => set("default_formats", formats.includes(format)
      ? formats.filter((value) => value !== format)
      : [...formats, format]),
  };
}

export function videoFinishingSettings(values: Record<string, unknown>, set: SettingsSet) {
  return {
    autoFraming: values.framing_approval === "auto",
    setAutoFraming: (enabled: boolean) => set("framing_approval", enabled ? "auto" : "review"),
    setLutIntensity: (value: string) => set("lut_intensity", Number(value)),
  };
}

const macEncoders = ["auto", "videotoolbox"];
const windowsEncoders = ["auto", "nvenc", "mf"];

export function videoEncoderSettings(platform: string, saved: unknown) {
  const choices = platform === "macos" ? macEncoders : windowsEncoders;
  const value = typeof saved === "string" && choices.includes(saved) ? saved : "auto";
  return { choices: choices as readonly string[], value };
}

/** Scoped file and folder selection for settings controls. */
export function useVideoSettingsPickers(dir: string | null, set: SettingsSet, onError: (message: string) => void) {
  const scope = useRef<object | null>(null);
  useLayoutEffect(() => {
    const token = {};
    scope.current = token;
    return () => { if (scope.current === token) scope.current = null; };
  }, [dir, set]);
  const pick = async (key: string, select: () => Promise<string | null>) => {
    const token = scope.current;
    if (!token) return;
    try {
      const value = await select();
      if (value && scope.current === token) await set(key, value);
    } catch (error) {
      if (scope.current === token) onError(errorMessage(error));
    }
  };
  return {
    pickLut: (title: string) => pick("lut_path", () => pickFile(title, ["cube", "3dl"])),
    pickExportFolder: (title: string) => pick("export_dir", () => pickFolder(title)),
    clearLut: () => set("lut_path", null),
  };
}
export function useVideoStorageSettings(dir: string | null, set: SettingsSet, onCleared: (freedBytes: number) => void, onError: (message: string) => void) {
  const [storage, setStorage] = useState<Awaited<ReturnType<typeof storageStatus>> | null>(null);
  const scope = useRef<object | null>(null);
  const pending = useRef(false);
  useLayoutEffect(() => {
    const token = {};
    scope.current = token;
    pending.current = false;
    setStorage(null);
    return () => { if (scope.current === token) scope.current = null; };
  }, [dir, set]);
  useEffect(() => {
    const token = scope.current;
    let live = true;
    storageStatus(dir ?? undefined).then((value) => { if (live && scope.current === token) setStorage(value); })
      .catch(() => { if (live && scope.current === token) setStorage(null); });
    return () => { live = false; };
  }, [dir, set]);
  const chooseProjectFolder = async (title: string) => {
    const token = scope.current;
    try {
      const value = await pickFolder(title);
      if (value && token && scope.current === token) await set("project_root", value);
    } catch (error) {
      if (token && scope.current === token) onError(errorMessage(error));
    }
  };
  const clear = async () => {
    const token = scope.current;
    if (!token || pending.current || !storage || storage.cache_bytes === 0) return;
    pending.current = true;
    try {
      const value = await storageClear(dir ?? undefined);
      if (scope.current === token) { setStorage((old) => old ? { ...old, cache_bytes: 0 } : old); onCleared(value.freed_bytes); }
    } catch (e) { if (scope.current === token) onError(errorMessage(e)); }
    finally { if (scope.current === token) pending.current = false; }
  };
  return { storage, chooseProjectFolder, clear };
}

export function useVideoGenerationSettings<Host>(initialPath: string, set: SettingsSet, interpret: (value: unknown) => Host, failed: (message: string) => Host, checking: Host, onError: (message: string) => void) {
  const [status, setStatus] = useState({ provider: set, value: checking });
  const [draft, setDraft] = useState<{ provider: SettingsSet; value: string } | null>(null);
  const path = draft?.provider === set ? draft.value : initialPath;
  const setPath = (value: string) => setDraft({ provider: set, value });
  const scope = useRef<object | null>(null);
  const sequence = useRef(0);
  const report = useRef({ interpret, failed });
  useLayoutEffect(() => { report.current = { interpret, failed }; }, [interpret, failed]);
  useLayoutEffect(() => {
    const token = {};
    scope.current = token;
    return () => { if (scope.current === token) scope.current = null; };
  }, [set]);
  useLayoutEffect(() => {
    setDraft((current) => current && (current.provider !== set || current.value.trim() === initialPath)
      ? null
      : current);
  }, [initialPath, set]);
  const check = useCallback(() => {
    const token = scope.current;
    if (!token) return;
    const request = ++sequence.current;
    const current = () => scope.current === token && sequence.current === request;
    void engineCall("genright.status", {})
      .then((value) => { if (current()) setStatus({ provider: set, value: report.current.interpret(value) }); })
      .catch((error) => { if (current()) setStatus({ provider: set, value: report.current.failed(errorMessage(error)) }); });
  }, [set]);
  useEffect(() => { check(); }, [check]);
  const commitPath = () => {
    if (draft?.provider === set && path.trim() !== initialPath) {
      return set("genright_engine_path", path.trim() || null);
    }
  };
  const pickPath = async (title: string) => {
    const token = scope.current;
    try {
      const value = await pickFile(title, null);
      if (value && token && scope.current === token) {
        setPath(value);
        await set("genright_engine_path", value);
      }
    } catch (error) {
      if (token && scope.current === token) onError(errorMessage(error));
    }
  };
  const setBudget = (value: string) => set("genright_budget_usd", value === "" ? null : Number(value));
  return { host: status.provider === set ? status.value : checking, path, setPath, check, commitPath, pickPath, setBudget };
}
