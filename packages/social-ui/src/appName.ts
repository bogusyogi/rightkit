// Product name shown in user-facing copy. Hosts call setHostAppName() once at startup.
let current = "App";

export function setHostAppName(name: string): void {
  current = name;
}

export function hostAppName(): string {
  return current;
}

// The apps that produce assets for this host ("A or B"), named where a brief waits for approval
// there. Empty (the default) names none.
let producers = "";

export function setHostProducerApps(names: string): void {
  producers = names;
}

export function hostProducerApps(): string {
  return producers;
}
