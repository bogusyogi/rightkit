/** The slice of a web `Storage` the hand-over reads and clears. */
export type LegacyStorage = Pick<Storage, 'getItem' | 'removeItem'>;

export interface LegacyStorageHandOverConfig {
  /** The storage the old values live in (for example a retired window's `localStorage`). */
  storage: LegacyStorage;
  /** The keys to hand over. Each one is read before `send` runs and removed only after `send` resolves. */
  keys: readonly string[];
  /**
   * Receives the raw stored string for every key (`null` when unset). The host parses and validates the values
   * and sends them to its native side; a rejection keeps the keys in place so the next start retries.
   */
  send: (values: Readonly<Record<string, string | null>>) => Promise<void>;
}

/**
 * One-time hand-over of values a retired renderer kept in web storage to the native side. Reads every key; if
 * none is set it does nothing and never calls `send`. Otherwise it calls `send` once with the raw values and,
 * only after that resolves, removes the keys. A failed `send` leaves the storage untouched.
 */
export async function handOverLegacyStorage({ storage, keys, send }: LegacyStorageHandOverConfig): Promise<void> {
  const values: Record<string, string | null> = {};
  for (const key of keys) values[key] = storage.getItem(key);
  if (keys.every((key) => values[key] === null)) return;
  await send(values);
  for (const key of keys) storage.removeItem(key);
}
