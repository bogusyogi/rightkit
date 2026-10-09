# Agent ownership policy hooks

`ownership-write-hook-cli.mjs` adds secondary feedback for Claude `Write`/`Edit`/`MultiEdit` & Codex `apply_patch`. Both adapters deny writes targeting ownership exception or platform trust records. `ownership-session-hook-cli.mjs` adds the current RightKit ownership index at session start. Hook feedback is fail-open for ambiguous edits; build admission remains authoritative.

Install the seven runtime assets & append the hook records to both host configs:

```sh
node rightkit/packages/build-control-policy/src/install-agent-policy.mjs
```

The installer writes verified assets into a content-addressed `content-<sha256>` directory under `D:\.rightkit-managed\agent-policy` on Windows or `/Volumes/D/.rightkit-managed/agent-policy` on Mac. Hook commands use that exact path, so each registration identifies the installed byte set. It preserves existing config fields/groups & skips duplicate records. Never register source checkout paths.

Append these groups to existing host hook arrays. Preserve all existing groups & commands. Substitute `<write-hook>` & `<session-hook>` with installer output paths, escaping backslashes for JSON.

Claude `settings.json` additions:

```json
{
  "PreToolUse": [{
    "matcher": "Write|Edit|MultiEdit",
    "hooks": [{ "type": "command", "command": "node <write-hook>" }]
  }],
  "SessionStart": [{
    "hooks": [{ "type": "command", "command": "node <session-hook>" }]
  }]
}
```

Codex `hooks.json` additions:

```json
{
  "PreToolUse": [{
    "matcher": "Bash|apply_patch|exec_command|functions\\.exec",
    "hooks": [{ "type": "command", "command": "node <write-hook>" }]
  }],
  "SessionStart": [{
    "hooks": [{ "type": "command", "command": "node <session-hook>" }]
  }]
}
```
