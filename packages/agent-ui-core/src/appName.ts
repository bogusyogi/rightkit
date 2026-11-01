// Product name shown in user-facing copy. Hosts call setHostAppName() once at startup.
let current = "App";

export function setHostAppName(name: string): void {
  current = name;
}

export function hostAppName(): string {
  return current;
}
