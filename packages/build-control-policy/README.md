# Agent ownership policy hooks

`ownership-write-hook-cli.mjs` adds secondary feedback for Claude `Write`/`Edit`/`MultiEdit` & Codex `apply_patch`. Both adapters deny writes targeting ownership exception or platform trust records. `ownership-session-hook-cli.mjs` adds the current RightKit ownership index at session start. Hook feedback is fail-open for ambiguous edits; build admission remains authoritative.

Install the fourteen runtime assets (`src/install-policy-assets.mjs:9-24`) & append the hook records to both host configs:

```sh
node rightkit/packages/build-control-policy/src/install-agent-policy.mjs
```

The installer writes verified assets into a content-addressed `content-<sha256>` directory under `D:\.rightkit-managed\agent-policy` on Windows or `~/Library/Application Support/RightKit/build-control/agent-policy` on Mac (`RIGHTKIT_CONTROL_ROOT` overrides; `packages/release/managed-roots.mjs:10-11`). Hook commands never name a `content-*` directory. They name a fixed launcher under the same root, so the registered command text is byte-identical across policy activations and an agent that hashes it (Codex `trusted_hash`) sees no change. Never register source checkout paths or `content-*` paths.

### Stable launchers & the active pointer

```text
agent-policy/
  active.json          {"schemaVersion": 1, "digest": "<sha256>"}, replaced atomically (temp file, flush, rename)
  launch/
    hook-launcher.mjs  shared core, a byte copy of src/hook-launcher.mjs
    write-hook.mjs     registered for the write/Bash hook
    session-hook.mjs   registered for SessionStart
  content-<sha256>/    verified assets, unchanged; superseded directories are kept
```

Install order is content directory, verified; launchers; then `active.json`. Launchers are written only when their bytes differ, and their bytes depend on neither a digest nor the root, so a re-install touches nothing it does not need to.

At run time the launcher reads `active.json`, requires a 64-character lowercase hex digest, resolves `content-<digest>/src/<fixed hook script>`, refuses any path whose real path leaves the root, and imports that script in the same node process (no child process). stdin, argv (`argv[1]` becomes the script, as for a direct run), stdout, stderr & the exit code reach the agent exactly as before.

A missing, unreadable or malformed pointer, an invalid digest, an absent or non-file target, or a path outside the root is refused with one `[HOOK:rightkit-policy-launcher:unavailable] <code>: ...` line on stderr, nothing on stdout, and exit 1. That is what `node <missing script>` already did. Claude Code documents exit 1 & a script that cannot start as non-blocking hook errors; Codex's hook documentation does not state this, so its handling is unverified. These hooks are advisory feedback & broker/build admission stays authoritative. The launcher never exits 0 on a failure and never exits 2, so a broken install cannot look like an allow decision or block every tool call. Denials (exit 2) come from the hook script and pass through unchanged; an error thrown by the script propagates unchanged.

### Registration & migration

Registration updates in place any existing RightKit command for the same hook that points into a `content-<sha256>` directory of this root (or of the legacy managed root's `agent-policy`), or at the launcher with a different node path. Its position, group matcher & other fields stay; duplicate RightKit entries for that hook are dropped; a new group is appended only when none exists. Hooks that are not RightKit's, and every other field & group, are untouched. A config that already holds the launcher commands is not rewritten.

When this change is first activated, the agent asks to review the hooks one last time, because the command changes from a content-addressed path to the launcher path. Later policy activations leave the command unchanged and do not prompt again. The command also embeds the installing node's absolute path, so installing with a different node binary (for example after a Node upgrade moves it) changes the command and prompts once.

Old `content-*` directories are left in place.

### Manual registration

Append these groups to existing host hook arrays. Preserve all existing groups & commands. Substitute `<write-launcher>` & `<session-launcher>` with the installer's `writeLauncher` & `sessionLauncher` output paths, escaping backslashes for JSON.

Claude `settings.json` additions:

```json
{
  "PreToolUse": [{
    "matcher": "Write|Edit|MultiEdit",
    "hooks": [{ "type": "command", "command": "node <write-launcher>" }]
  }],
  "SessionStart": [{
    "hooks": [{ "type": "command", "command": "node <session-launcher>" }]
  }]
}
```

Codex `hooks.json` additions:

```json
{
  "PreToolUse": [{
    "matcher": "Bash|apply_patch|exec_command|functions\\.exec",
    "hooks": [{ "type": "command", "command": "node <write-launcher>" }]
  }],
  "SessionStart": [{
    "hooks": [{ "type": "command", "command": "node <session-launcher>" }]
  }]
}
```
