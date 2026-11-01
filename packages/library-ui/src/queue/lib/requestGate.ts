/**
 * Latest-wins gate for overlapping requests: each `begin()` invalidates every earlier
 * token, so a late response from a superseded request can check `isCurrent` and drop out.
 */
export interface RequestGate {
  begin(): number;
  latest(): number;
  isCurrent(token: number): boolean;
}

export function createRequestGate(): RequestGate {
  let latestToken = 0;
  return {
    begin() {
      latestToken += 1;
      return latestToken;
    },
    latest() {
      return latestToken;
    },
    isCurrent(token) {
      return token === latestToken;
    },
  };
}
