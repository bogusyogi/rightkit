1. **High — signing safeguards are overstated (§8).** [Legion release-candidate:60](/Volumes/D/claude/legion/.github/workflows/release-candidate.yml:60) permits branch rehearsals, interpolates inputs directly into shell & rejects ordinary release reruns at line 65. Replace lines 113–115 with:
   > “These are required safeguards, not verified workflow properties. Release rehearsals can sign branch builds through protected environments. Pass inputs through environment variables before validation. Never infer rerun or signing-change authority from this runbook.”

2. **High — blanket test skipping can manufacture green (§6).** [Runbook:90](/Volumes/D/claude/docs/rules/public-ci.md:90) makes missing assets optional by default. Replace:
   > “Skip only explicitly optional coverage. Missing assets for required checks must fail or report unproven coverage; provision required assets through approved mechanisms.”

3. **High — last-green claims contradict Pulse (§3, §11).** [Pulse CI:37](/Volumes/D/claude/pulse/.github/workflows/ci.yml:37) uses `github.event.before`; [template:32](/Volumes/D/claude/rightkit/packages/git/templates/profiles/rust-hybrid/ci.yml:32) uses markers. Replace line 43:
   > “RightKit, RustRight & Legion use per-OS/Node/ref markers; Pulse’s rendered workflow still compares consecutive pushes. Markers record successful gate progress before later artifacts/jobs finish, not whole-workflow success. Missing/invalid/nonancestor markers force Rust selection. PRs use their base SHA, never markers. Marker keys intentionally include SHA/run/attempt.”

4. **High — development provenance is wrong (§7).** [Legion CI:159](/Volumes/D/claude/legion/.github/workflows/ci.yml:159) permits dispatch; [Pulse CI:193](/Volumes/D/claude/pulse/.github/workflows/ci.yml:193) uploads independently of QA. Replace lines 94 & 101:
   > “Pulse builds on main pushes: Windows uploads can precede failed QA; Developer ID Mac signing waits for CI & Mac build, without notarization. Legion produces unsigned Windows & `dev-mac-unsigned-<sha>` artifacts on main pushes or manual dispatches of other refs. Verify repository, run, SHA, platform, gate results & signature before installation.”

5. **High — identity guidance overclaims Apple (§7).** Quotes match, but [TN3127](https://developer.apple.com/documentation/technotes/tn3127-inside-code-signing-requirements) describes certificate constraints & differing signing requirements—not merely identifier/team. Replace lines 96, 101 & 105:
   > “Preserve compatible designated requirements, identifiers, entitlements & installation location. Compare old/new signatures & helper identities before diagnosing repeated prompts. Signing location alone does not determine identity; repeated prompts do not prove identity changed. Do not reset permissions or change signing as automatic remediation.”

6. **Medium — cache saves are misrepresented (§4).** [RightKit CI:109](/Volumes/D/claude/rightkit/.github/workflows/ci.yml:109) contradicts line 48. Replace:
   > “Rust-cache restores configured Cargo/target data; non-PR runs can save after failure, but exact hits are not refreshed. DerivedData uses OS/runner plus Cargo.lock, Package.resolved & project.pbxproj hashes, with prefix fallback; successful PR jobs can save it under their merge ref. PRs neither restore nor save last-green markers. Completed jobs may save caches before another job fails.”

   [GitHub cache behavior](https://docs.github.com/en/actions/reference/workflows-and-actions/dependency-caching).

7. **Medium — shared-key/profile claims are false (§4, §7).** [Pulse CI:111](/Volumes/D/claude/pulse/.github/workflows/ci.yml:111), [166](/Volumes/D/claude/pulse/.github/workflows/ci.yml:166) & [219](/Volumes/D/claude/pulse/.github/workflows/ci.yml:219) differ. Replace lines 55, 61–62 & 108:
   > “Shared-key is only part of rust-cache identity: toolchain, environment, manifests & locks also contribute. Pulse adds distinct CI/Mac-dev/Windows-dev suffixes; dev scripts use release builds. Lock changes can restore older dependency caches. Rotation is not the only refresh mechanism. Incremental directories are pruned before saving.”

   [Pinned action implementation](https://raw.githubusercontent.com/Swatinem/rust-cache/6323deb102c322ba6fcbdcafc7e3dddab59af2b6/src/cleanup.ts).

8. **Medium — configured caches may provide no benefit.** [Renderer:1786](/Volumes/D/claude/rightkit/packages/git/manifest.mjs:1786), [Pulse gate:210](/Volumes/D/claude/pulse/scripts/gate.sh:210), [Legion gate:40](/Volumes/D/claude/legion/scripts/ci/right-git-ci.sh:40). Add:
   > “All four manifests enable incremental CI, clearing RUSTC_WRAPPER; sccache setup does not prove use. Pulse’s custom DerivedData path misses this cache. Legion compiles unconditionally despite conditional cache restoration; requiredForGate must reflect that.”

9. **Medium — concurrency needs workflow scope (§3).** [Renderer:724](/Volumes/D/claude/rightkit/packages/git/manifest.mjs:724), [Legion CI:11](/Volumes/D/claude/legion/.github/workflows/ci.yml:11). Replace lines 14, 37–39:
   > “Pulse/RightKit/RustRight CI preserve running default-branch pushes; newest pending replaces older pending. Legion cancels superseded main runs, including same-ref dispatch collisions. All four cancel superseded runs per PR; separate PRs run concurrently. Release-candidate groups serialize by version; Legion regenerate serializes separately. Cache warming never prohibits stopping harmful runs.”

10. **Medium — roles/execution contradict existing policy.** [RightKit:35](/Volumes/D/claude/docs/rules/rightkit.md:35) allows bounded local Windows work; line 69 says cancel superseded runs. Replace lines 11–12:
    > “Follow rightkit.md’s execution bindings & explicit local exceptions. Roles constrain already-authorized work; they do not authorize branches, chats, pushes, merges or dispatches. Follow github-access.md for fetch/reconciliation, explicit staging & authentication. Reconcile cancellation-policy differences explicitly.”

11. **Medium — PR/skip guidance can waste runs (§1, §3).** Replace lines 22 & 40:
    > “PR CI tests its event’s base/head merge, not necessarily current main; red main does not automatically fail every PR. Use skip directives only when the entire push needs no validation & required checks permit skipping.”

    [GitHub events](https://docs.github.com/en/actions/reference/workflows-and-actions/events-that-trigger-workflows#pull_request), [skip rules](https://docs.github.com/en/actions/how-tos/manage-workflow-runs/skip-workflow-runs).

12. **Medium — waiting/limits mix distinct budgets (§5, §9).** Replace:
    > “gh run watch polls; use one observer bound to exact run/SHA. Desktop authentication generally shares 5,000 requests/hour/user; GITHUB_TOKEN normally has 1,000/hour/repository. Honor Retry-After/reset headers & bounded backoff. Free-plan concurrency is 20/5; verify owner’s actual plan. Cache storage defaults to 10 GB; increases can incur charges.”

    [CLI](https://cli.github.com/manual/gh_run_watch), [REST guidance](https://docs.github.com/en/rest/using-the-rest-api/best-practices-for-using-the-rest-api), [limits](https://docs.github.com/en/actions/reference/limits), [cache limits](https://docs.github.com/en/actions/reference/workflows-and-actions/dependency-caching).

13. **Medium — §11 mixes configuration with unsupported live state.** Replace affected cells:
    > “Pulse gaps: stale change detection; ineffective DerivedData cache. RightKit workflows: CI plus manual ffmpeg-windows; green status requires run evidence. RustRight: no devArtifact configured; local-install status unverified. Legion: unsigned push/dispatch artifacts; regenerate also uses Ubuntu. SellRight: CI, CodeQL & refresh-dependencies workflows; no manifest. Lightcraft/static-site configuration unverified. Attach run evidence to the 29-minute measurement.”

    Evidence: [FFmpeg:8](/Volumes/D/claude/rightkit/.github/workflows/ffmpeg-windows.yml:8), [SellRight CI:3](/Volumes/D/claude/sellright/.github/workflows/ci.yml:3), [regenerate:27](/Volumes/D/claude/legion/.github/workflows/regenerate.yml:27).