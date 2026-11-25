import assert from "node:assert/strict";
import test from "node:test";
import { socketProblem } from "./seam-broker.mjs";

// A fake file system: each path is { type: "socket" | "link" | "file", uid, to? }.
const fakeFs = (paths) => ({
  lstat: (p) => { const e = paths[p]; if (!e) throw Object.assign(new Error("missing"), { code: "ENOENT" }); return { isSocket: () => e.type === "socket", isSymbolicLink: () => e.type === "link", uid: e.uid }; },
  realpath: (p) => { let at = p; for (let i = 0; i < 8 && paths[at]?.type === "link"; i += 1) at = paths[at].to; return at; },
  uid: 501,
});

test("the broker socket may be named through a link (the Mac's managed root); the socket it reaches is checked, and the link's owner too", () => {
  const sock = "/Users/a/Library/Application Support/RightKit/build-control/broker.sock";
  assert.equal(socketProblem("/Volumes/D/.rightkit-managed/broker.sock", fakeFs({ "/Volumes/D/.rightkit-managed/broker.sock": { type: "link", uid: 501, to: sock }, [sock]: { type: "socket", uid: 501 } })), null);
  assert.equal(socketProblem(sock, fakeFs({ [sock]: { type: "socket", uid: 501 } })), null);
  assert.match(socketProblem("/m/broker.sock", fakeFs({ "/m/broker.sock": { type: "link", uid: 501, to: "/x/file" }, "/x/file": { type: "file", uid: 501 } })), /\/m\/broker\.sock -> \/x\/file is not a socket/);
  assert.match(socketProblem("/m/broker.sock", fakeFs({ "/m/broker.sock": { type: "link", uid: 501, to: sock }, [sock]: { type: "socket", uid: 0 } })), /owned by another user \(uid 0\)/);
  assert.match(socketProblem("/m/broker.sock", fakeFs({ "/m/broker.sock": { type: "link", uid: 0, to: sock }, [sock]: { type: "socket", uid: 501 } })), /a link owned by another user/);
  assert.match(socketProblem("/m/broker.sock", fakeFs({ "/m/broker.sock": { type: "file", uid: 501 } })), /is not a socket/);
  assert.match(socketProblem("/m/none.sock", fakeFs({})), /cannot connect to the broker at \/m\/none\.sock \(ENOENT\)/);
});
