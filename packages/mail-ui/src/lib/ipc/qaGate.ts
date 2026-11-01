type ViteEnv = Record<string, string | boolean | undefined>;

function viteEnv(): ViteEnv {
  return (
    (import.meta as unknown as { env?: ViteEnv }).env ?? {}
  );
}

function qaRuntimeAllowed(env: ViteEnv = viteEnv()): boolean {
  return env.DEV === true || env.DEV === "true";
}

export function qaEnabledFrom(
  search: string,
  envValue: string | undefined,
  allowQa = qaRuntimeAllowed(),
): boolean {
  if (!allowQa) return false;
  const params = new URLSearchParams(search.startsWith("?") ? search.slice(1) : search);
  return params.get("qa") === "1" || envValue === "1";
}

export function browserQaEnabled(): boolean {
  const env = viteEnv();
  const search =
    typeof globalThis.location === "object" && globalThis.location
      ? globalThis.location.search
      : "";
  return qaEnabledFrom(search, env.VITE_RIGHTKIT_QA as string | undefined);
}
