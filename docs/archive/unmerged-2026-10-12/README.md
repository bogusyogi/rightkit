# Unmerged work, archived before the branch clean-up (2026-10-12)

Adrian ordered every repository clean: no stashes, no branches except main, nothing else on origin. Work that was not on main is kept here as patches so nothing is lost. Apply with `git am <file>.patch` (branches) or `git apply <file>.patch` (stash). Each patch is against the main of its day and may need rebasing.

| Patch | What it was | State when archived |
|---|---|---|
| stash-0-on-7290a74d.patch | rightkit-asr whisper control, rightkit-llm error and provider changes, rightkit-shell lifecycle (2026-10-11) | never committed |
| mac_devtools-policy.patch | DevTools policy, content-only allowance, release gate | PR 27 closed; superseded by PRs 43, 45, 46 |
| mac_process-coordination.patch | rightkit-process identity-checked adopt, machine-wide holder lock, caller lease | never compiled, kept off main |
| mac_under-test-guard.patch | rightkit-service/models refuse the real per-user folder under test | never compiled, kept off main |
| win_devtools-native-refuse.patch | rightkit-shell Windows refuses a webview whose DevTools cannot be disabled | partly merged via PRs 43 and 45 |
| win_ffmpeg-decode-components.patch | rightkit-media Windows toolchain runtime components and notices | not merged |
| win_secrets-legacy-lock-test.patch | rightkit-secrets legacy-lock name test | PR 54 closed; same fix landed on main |
| proposal_postright-jiff-allowlist.patch | ownership allowlist entry cargo:jiff for PostRight | waits for Adrian's trust-sync |
