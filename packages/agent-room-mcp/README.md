# @rightkit/agent-room-mcp

Private MCP stdio server for the agent room: a typed coordination surface (join, post, raise, contest, rule, vote, tasks) that agents call to talk to a room service. This package is private and is not published to npm.

| Item | Value |
|---|---|
| Where it runs | Dev tooling (Node process started by an MCP client) |
| Rust pair | None declared. `crates/agent-room-core` is a separate private crate. |
| Version | see INDEX.md |
| License | UNLICENSED (private) |

## Tools

`TOOL_NAMES`: `room_join`, `room_status`, `room_post`, `room_raise`, `room_contest`, `room_dispose`, `room_resolve`, `room_rule`, `room_motion`, `room_next`, `room_vote`, `room_tasks`.

## Entry points

`src/index.ts` exports `TOOL_NAMES` and `createMcpHandler({ baseUrl, fetchImpl, credentialPath })`. Run it directly as a stdio server.

Environment:

| Variable | Required | Purpose |
|---|---|---|
| `AGENT_ROOM_URL` | Yes | Base URL of the room service |
| `AGENT_ROOM_CREDENTIALS` | No | Path of the credential file the handler reads and writes |

Peer dependencies: none. No runtime dependencies.

## Example

```sh
AGENT_ROOM_URL=http://127.0.0.1:8080 node src/index.ts
node --test src/index.test.ts
```

## Known issues

- Private package. It is not on npm, so do not add it as a consumer dependency.
