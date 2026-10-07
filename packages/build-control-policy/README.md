# Write-time ownership feedback

`src/ownership-write-hook-cli.mjs` accepts one host event as JSON on stdin. It
reconstructs Claude `Write`/`Edit`/`MultiEdit` and Codex `apply_patch` updates,
then blocks source fingerprints whose occurrence count increases. Edits that
reduce duplicates are allowed. A malformed patch that clearly adds a known
fingerprint is blocked. The hook blocks write-shaped commands targeting
`.rightkit/ownership-exceptions.json` and the platform trust registry; opaque
shell writes remain governed by authoritative build admission.

The adapter loads its packaged `src/ownership-fingerprints.json` snapshot. The
catalog equality test keeps this snapshot aligned with the release catalog.

Append these entries to existing hook arrays, keeping current groups and
commands. Source checkout paths are `D:/Claude/tools/rightkit` on Windows and
`/Volumes/D/claude/tools/rightkit` on Mac.

Claude PreToolUse append: `{"matcher":"Write|Edit|MultiEdit","hooks":[{"type":"command","command":"node \"D:/Claude/tools/rightkit/packages/build-control-policy/src/ownership-write-hook-cli.mjs\""}]}`

Claude SessionStart append: `{"hooks":[{"type":"command","command":"node \"D:/Claude/tools/rightkit/packages/build-control-policy/src/ownership-session-hook-cli.mjs\""}]}`

Codex PreToolUse append: `{"matcher":"Bash|apply_patch|exec_command|functions\\.exec","hooks":[{"type":"command","command":"node \"D:/Claude/tools/rightkit/packages/build-control-policy/src/ownership-write-hook-cli.mjs\""}]}`

Codex SessionStart append: `{"hooks":[{"type":"command","command":"node \"D:/Claude/tools/rightkit/packages/build-control-policy/src/ownership-session-hook-cli.mjs\""}]}`

For Mac, replace `D:/Claude/tools/rightkit` in command strings with
`/Volumes/D/claude/tools/rightkit`. Codex adds JSON `additionalContext` to
session context.
