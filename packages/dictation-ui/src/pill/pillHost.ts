// The pill windows' access to the host: `PillClient` from `configureDictationHost({ pill })`.
// `listenPill` keeps the `{ payload }` handler shape the pill listeners were written against.
import { pillClient, type PillEventMap, type Unsubscribe } from "../host.js";

export { pillClient };

export function listenPill<K extends keyof PillEventMap>(
  event: K,
  handler: (event: { payload: PillEventMap[K] }) => void,
): Promise<Unsubscribe> {
  return pillClient().on(event, (payload) => handler({ payload }));
}
