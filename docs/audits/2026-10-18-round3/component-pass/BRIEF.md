# Component pass brief (RightKit round 2, 2026-10-18)
You are fixing ALL open gaps for ONE RightKit component. EDIT-ONLY: never run cargo build/check/test/clippy, npm install/test, or any build. The main chat compiles later.

Tree: D:\scratch\rk-gaps-work\c2\<comp>\tree is a plain git repo of RightKit at ea691005 (baseline commit "base"). Edit ONLY inside it, and only inside your component's directory (crates/rightkit-<comp>/ or packages/<comp>/) plus Cargo.toml/Cargo.lock/package.json entries strictly needed. Never touch D:\Claude\rightkit.

Steps:
1. Read pinned issue bogusyogi/rightkit#771 (SETTLED DECISIONS). Anything that conflicts with it is REJECTED, not implemented.
2. Issues: <comp>\issues.txt, PLUS run `gh issue list -R bogusyogi/rightkit --state open --search "<crate-name> in:title" --limit 100` and `--search "<crate-name> in:body"` to catch any you were not given. Read each with `gh issue view <n> -R bogusyogi/rightkit --comments`.
3. Prior patches for this component (if any) are in <comp>\patches\. They were written against older main; use them as reference and port the parts still missing. Don't apply them blindly.
4. Implement every in-scope gap fully: public API, docs comment, unit tests, CHANGELOG entry under a new Unreleased/next-version heading, and a semver-correct version bump in the crate's Cargo.toml (breaking → minor bump for 0.x). Keep platform code cfg-gated correctly for Windows, macOS and Linux. Match the surrounding code style.
5. If a gap needs a brand-new crate, a cross-component design decision, or Adrian's input, mark it DEFERRED with the exact reason; do not invent it.
6. Write outputs ONLY inside your own <comp> folder. Finish with: `cd tree && git add -A && git diff --cached HEAD > ..\<comp>.patch` (keep it LF), and write ..\REPORT.txt with one line per issue:
   DONE #n <what was added> | PARTIAL #n <done>; missing: <what> | DEFERRED #n <reason> | REJECTED #n <#771 item> | ALREADY #n <where it already exists on main>
   plus the new version and a list of public API changes (breaking marked).
Return the REPORT.txt content as your final message.

## Added rules (after batch 1)
- Prefer NON-BREAKING designs: add new methods, types or functions rather than adding variants to exhaustive enums, pub fields to existing structs, or removing Copy. A breaking bump forces every dependent crate to republish; take it only when no additive design exists, and say why.
- The issue list in your prompt is a MINIMUM. Every open issue for your component (issues.txt plus the gh searches) is in scope: mark each DONE, ALREADY (with its location), PARTIAL, DEFERRED (with a reason) or REJECTED (#771). Never mark one "not assigned".
