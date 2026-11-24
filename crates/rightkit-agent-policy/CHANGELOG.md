# Changelog

## 0.2.3

Safety fixes to `destructive::catastrophic_reason`, the write-then-run guard (CodeRight #457 items 1, 2 and 4). Each command below passed 0.2.2 and is refused now:

- Groups: a redirection after a `( ... )` subshell or `{ ...; }` brace group applies to every command in the group, so the text the group prints is a write to that file: `( echo 'rm -rf /*' ) > x.sh && sh x.sh`, `{ echo 'rm -rf /*'; } > x.sh && sh x.sh`.
- Shell code strings: the clauses of a shell's `-c` string (`sh`, `bash -lc`, ...; behind wrappers too) are linked in sequence with the clauses around it, so a file written inside the string and run after it is caught: `sh -c "echo 'rm -rf /*' > x.sh" && sh x.sh`.
- Wrappers: behind a wrapper (`flock`, `su`, `timeout`, `nice`, `sudo`, ...) any word naming the file just written counts as the program run, since which word a wrapper runs depends on the wrapper: `echo 'rm -rf /*' > x.sh && su builder x.sh`, `... && flock /tmp/lock x.sh`. Before, only the word after numeric arguments counted. Trade-off: a wrapper that only reads such a file (`sudo cat x.sh` after writing a catastrophic command into it) is refused too.
- Still a listed limit (item 3): a name the guard cannot see (`split`'s derived output files) and a program that prints the command itself (`awk 'BEGIN{print "rm -rf /*"}' > x.sh`). Those need the check at the exec boundary.
- New end-to-end test `tests/destructive_nested_e2e.rs`: eight write-then-run forms refused, seven ordinary commands of the same shapes allowed. Fails on 0.2.2 (seven of the eight are allowed), passes here. The private `wrapped_program` helper is gone.

## 0.2.2 (2026-11-16)

- The package no longer ships `src/auto_approval.rs`, a tool-call approval classifier (`AutoApprovalClassifier`, `AutoApprovalCache`) that `src/lib.rs` never declared and nothing called. No public item, feature or behaviour changes; the compiled library is unchanged.

## 0.2.1

Additive: every setting below is once per process, set by the host at startup, and the default behaviour is unchanged for hosts that set none. They let CodeRight's `crates/policy` copy be replaced by this crate with byte-identical decisions.

- New `set_state_dir_name(name) -> bool` (default `.rightkit`, `DEFAULT_STATE_DIR_NAME`), `state_dir_name()`, `default_read_deny()` and `default_write_deny()`. Safety: the runtime-folder deny glob `**/<state-dir>/runtime/**` (the folder holds the daemon discovery file with the API token) was hard-coded to `.rightkit`; it now follows the setting in `DenyGlobSet::new`, `with_additional_patterns`, `PermissionEngine::new`, `PermissionEngine::with_pdp` and `shell_withholds_read`. `DEFAULT_READ_DENY` and `DEFAULT_WRITE_DENY` stay as they were (default state folder); the two functions return the lists in effect. The setting is separate from `rightkit-agent-config`'s `set_state_dir_name` (this crate cannot depend on it), so a host using both sets both. CodeRight calls `set_state_dir_name(".coderight")`.
- New `set_host_product_name(name) -> bool` (default `host`, `DEFAULT_HOST_PRODUCT_NAME`): wording of the owning-process denial reasons. Reasons read "the <name> runtime itself", "<name lower-cased> stop/serve/restart/service control" and "target is the <name lower-cased> process by name". The outcome is unchanged and the protected set still comes from `set_protected_process_name`. CodeRight calls `set_protected_process_name("coderight")` and `set_host_product_name("CodeRight")`.
- New `set_fail_closed_hint(hint) -> bool` (default none) and `fail_closed_hint()`: `PolicyDecisionPoint::fail_closed` appends the hint to the deny rule's text after one space. CodeRight calls `set_fail_closed_hint("Run `coderight doctor` to check it")`, which gives its text "...(the policy is read only when a session starts). Run `coderight doctor` to check it".
- Not changed: CodeRight's policy has no `list_mcp_sources` capability class (this crate classes it read-only); a host with no such tool is unaffected.

## 0.2.0

- Depends on `rightkit-agent-engine-protocol` 0.4.0 (additive events and fields, see its changelog). Breaking for dependents: the crate exposes protocol types in its public API, so those types are now the 0.4 ones.
- Synced with CodeRight's newer engine code (CodeRight `crates/policy`, `crates/coderight-legion-policy`), so CodeRight can use this crate instead of its copy. `auto_approval.rs` is not part of CodeRight and stays as it was. The `legion` feature needed no change: its files match CodeRight's apart from names.
- `destructive::catastrophic_reason` is the nested-shell guard: it parses the command into clauses and checks the code a clause hands to a shell, `eval`, `xargs`, `find -exec`, wrappers (`sudo`, `timeout`, ...), here-documents, and text a clause writes to a file that a later clause runs or copies. Safety fix: many catastrophic spellings the old substring guard missed are refused regardless of mode or trust.
- `PolicyDecisionPoint::from_yaml_like` is strict (safety fix): an unknown or misspelt key, unknown effect or condition operator, malformed `- id:` line, rule without `effect:` or `tool:`, repeated key or rule id, and a text with no rule are errors instead of being skipped or turned into `Ask`/`Eq`. Tool names in rules and requests compare case-insensitively and `bash` matches the `shell` tool.
- New `PolicyDecisionPoint::fail_closed` and `POLICY_INVALID_RULE_PREFIX`: the deny-all policy a host installs when a configured policy cannot be loaded (recovery paths ask instead of deny).
- `PermissionEngine`: an ABAC Allow no longer skips the destructive and test-file escalation outside Bypass (safety fix; `source` stays `abac:<id>`). A Deny reason says what to do next (Plan mode names `exit_plan_mode`).
- `DenyGlobSet` construction refuses an absolute glob (new `DenyGlobError::AbsolutePattern`, with a `**/<name>` suggestion), since deny globs match workspace-relative paths and an absolute one protected nothing (safety fix). New `shell_withholds_read` (state folder `.rightkit`) and `shell_withholds_read_in(rel, is_dir, state_dir_name)`; CodeRight calls the latter with `".coderight"`.
- `net_guard`: `resolve_browser_target_with_trust_and_allowlist` returns the validated addresses, so a caller connects to exactly what was checked (no second resolution that a TTL-0 rebinding answer could change).
- Provenance: new `ContentOrigin::UntrustedWorkspace` (content of a workspace trusted for one run only; a durable-sink origin that asks), `ContentOrigin::is_remote`, and `shell_durable_write_is_authorized` (only remote content makes a shell command a durable sink). Breaking for exhaustive matches on `ContentOrigin`.
- Shell command classification: `bash -c` is recognised only for options before the first operand; the `/dev/null` sink check needs the whole target to be the sink; `sed` and `sort` are routine only when they cannot write or run a program (`sed_is_read_only`; `sort --output`/`--compress-program`) (safety fixes). Under the `sandboxed` argument of `evaluate_shell_command_in` the shell's own composition (variables, substitutions, subshells, loops, conditionals, redirects to files) and ordinary file/text tools and checkers are routine; the default (`sandboxed = false`) is unchanged except for the fixes above and multi-piece double-quoted literal strings.
- Unit tests are not ported (end-to-end tests only).

## 0.1.4

- Breaking for dependents: a dependency requirement was raised to a semver-breaking version.

## 0.1.3 (unpublished)

- `PayloadDigest::of` is the `rightkit-approvals` 0.2.0 core's `bind_of` over the tool name and the payload, byte for byte the digest it computed itself (each part length-prefixed, SHA-256, lowercase hex); the private copy is deleted. Decision 3 of the 2026-11-11 shape review (docs/audits/2026-11-11-shape-matrix/README.md, "Approvals") taken on the recommended option, pending the owner's confirmation; design note: docs/audits/2026-11-11-shape-matrix/reports/approvals-core-design.md.
- Requires `rightkit-agent-engine-protocol` 0.2.0, whose approval records carry the approvals core's `Binding` (mandatory digest) instead of an optional `payload_digest`. Decision 4 of the 2026-11-11 shape review (docs/audits/2026-11-11-shape-matrix/README.md, "Agent engine") taken on the recommended option, pending the owner's confirmation; design note: docs/audits/2026-11-11-shape-matrix/reports/agent-engine-contract-design.md.

## 0.1.2

- Publishes the source changes made since 0.1.1 was published under the same version number (src/capabilities.rs, src/denial_tracking.rs, src/engine.rs, src/lib.rs, src/provenance.rs, src/shell_command.rs, src/trust.rs).

## 0.1.1 (unpublished)

- `parse_capability`: `list_mcp_sources` is a read-class tool like `list_mcp_tools` (rightkit#1245), so the new tool is not treated as an unknown, gated class.
- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- Synced with CodeRight: literal session command grants (`Rule::session_command_grant`), `EffectClass` / `ApprovalClass` / `effect_class_for_tool`, `TrustPolicy::bypass_refusal`, `evaluate_shell_command_in` / `classify_shell_command` (interpreters and test runners are routine under an enforcing OS sandbox), memory writes from any non-session origin ask, an ABAC Allow no longer relaxes the untrusted-workspace clamp; the auto-approval classifier and cache are removed and `transcribe` is no longer a media capability alias.
- Feature `legion`: effect adapter and authorization receipts binding agent policy decisions to sandbox effects, moved from CodeRight `coderight-legion-policy`.
- First RightKit release, moved from `coderight-policy` (engine).
