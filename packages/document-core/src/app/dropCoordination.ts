import { listen } from "@tauri-apps/api/event";
import { isTauri } from "@rightkit/platform-ui";

export interface NativeDropPosition {
  x: number;
  y: number;
}

export interface NativeDropPayload {
  paths: string[];
  position?: NativeDropPosition;
}

export interface NativeDropClaimContext {
  isCancelled: () => boolean;
  signal: AbortSignal;
}

type NativeDropTarget = {
  host: HTMLElement;
  consume: (
    payload: NativeDropPayload,
    context: NativeDropClaimContext,
  ) => Promise<readonly string[]>;
};

const targets = new Set<NativeDropTarget>();
const CLAIM_TIMEOUT_MS = 4_000;

export const isTauriRuntime = isTauri;

export function normalizeDropPath(path: string): string {
  const normalized = path.replace(/\\/g, "/");
  // Drive-letter + UNC paths are case-insensitive; preserve POSIX case.
  return /^[A-Za-z]:\//.test(normalized) || normalized.startsWith("//")
    ? normalized.toLowerCase()
    : normalized;
}

export function isImagePath(path: string): boolean {
  return /\.(?:png|jpe?g|webp|gif|svg)$/i.test(path);
}

function activeTarget(): NativeDropTarget | undefined {
  for (const target of targets) {
    if (!target.host.isConnected) continue;
    const pane = target.host.closest<HTMLElement>("[data-vr-pane-state]");
    if (!pane || pane.dataset.vrPaneState === "active") return target;
  }
  return undefined;
}

export function registerMarkdownDropTarget(
  host: HTMLElement,
  consume: NativeDropTarget["consume"],
): () => void {
  const target = { host, consume } satisfies NativeDropTarget;
  targets.add(target);
  return () => targets.delete(target);
}

function parseNativeDropPayload(value: unknown): NativeDropPayload | null {
  if (!value || typeof value !== "object") return null;
  const candidate = value as { paths?: unknown; position?: unknown };
  if (!Array.isArray(candidate.paths)) return null;
  const paths = candidate.paths.filter(
    (path): path is string => typeof path === "string" && path.length > 0,
  );
  if (!paths.length) return null;
  const rawPosition = candidate.position;
  const position =
    rawPosition &&
    typeof rawPosition === "object" &&
    typeof (rawPosition as { x?: unknown }).x === "number" &&
    typeof (rawPosition as { y?: unknown }).y === "number"
      ? { x: (rawPosition as { x: number }).x, y: (rawPosition as { y: number }).y }
      : undefined;
  return { paths, position };
}

function uniquePaths(paths: readonly string[]): string[] {
  const seen = new Set<string>();
  return paths.filter((path) => {
    const normalized = normalizeDropPath(path);
    if (seen.has(normalized)) return false;
    seen.add(normalized);
    return true;
  });
}

async function consumeWithTimeout(
  target: NativeDropTarget,
  payload: NativeDropPayload,
): Promise<readonly string[]> {
  const controller = new AbortController();
  let timer: ReturnType<typeof setTimeout> | undefined;
  const context: NativeDropClaimContext = {
    isCancelled: () => controller.signal.aborted || !target.host.isConnected,
    signal: controller.signal,
  };
  const timeout = new Promise<readonly string[]>((resolve) => {
    timer = setTimeout(() => {
      controller.abort();
      resolve([]);
    }, CLAIM_TIMEOUT_MS);
  });
  try {
    let claim: Promise<readonly string[]>;
    try {
      claim = Promise.resolve(target.consume(payload, context));
    } catch {
      return [];
    }
    return await Promise.race([claim, timeout]).catch(() => []);
  } finally {
    if (timer) clearTimeout(timer);
  }
}

/** Route one brokered native drop, preserving source order for insertion & fallback opens. */
export async function routeNativeDrop(
  rawPayload: unknown,
  open: (path: string) => Promise<unknown> | unknown,
): Promise<void> {
  const parsed = parseNativeDropPayload(rawPayload);
  if (!parsed) return;
  const payload = { ...parsed, paths: uniquePaths(parsed.paths) };
  const imagePaths = payload.paths.filter(isImagePath);
  const target = imagePaths.length ? activeTarget() : undefined;
  const consumed = new Set<string>();

  if (target) {
    const claimed = await consumeWithTimeout(target, { ...payload, paths: imagePaths });
    for (const path of claimed) {
      const normalized = normalizeDropPath(path);
      if (imagePaths.some((candidate) => normalizeDropPath(candidate) === normalized)) {
        consumed.add(normalized);
      }
    }
  }

  for (const path of payload.paths) {
    if (consumed.has(normalizeDropPath(path))) continue;
    await open(path);
  }
}

export async function listenForNativeDrops(
  open: (path: string) => Promise<unknown> | unknown,
): Promise<() => void> {
  if (!isTauriRuntime()) return () => {};
  const unlisten = await listen<unknown>("native-drop", (event) => {
    void routeNativeDrop(event.payload, open);
  });
  return unlisten;
}

/** Test cleanup; also protects hot-reload from retaining stale editor targets. */
export function resetDropTargets(): void {
  targets.clear();
}
