# Round 3 evidence (2026-10-18), for the mandatory-set audit

- `component-pass/`: the 10-18 edit-only component pass. It has one `<comp>.patch` per component, all against ea691005, NOT compiled and NOT landed, plus the agent `REPORT-<comp>.txt` files, `BRIEF.md` (the instructions the agents got), `INTEGRATION-NOTES.txt` (known defects and decisions), `TRIAGE.txt` and `mac-unlanded-patches.txt` (45 older patches that never landed).
- `app-audits/`: each app's no-build RightKit gap audit on 10-18, measured against the published crates.

These patches are evidence for review only. Nothing here is applied to the source tree.
