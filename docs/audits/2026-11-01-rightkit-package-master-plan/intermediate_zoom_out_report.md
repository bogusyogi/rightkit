# Stage 8: Intermediate Zoom Out on design_draft v2

Verdict: NEEDS REVISION (1 Blocker, 6 Major, 3 Nit). All were resolved in v3, except M-4, which is contested. EGM allows at most one re-audit per stage; the resolutions below are recorded instead of a second re-audit.

| # | Finding | Resolution in v3 |
|---|---|---|
| B-1 | The private tier had no home; the procedure put private code into public `crates/`/`packages/` and the public catalog; "same qualification route" was wrong (public CI refuses private repos); three distribution mechanisms | Separate `rightkit-private` repo with its own catalog, qualified locally on Mac and Dell (§1 ③). One mechanism: git dependencies pinned to tagged commits. Gate support for these pins is Wave 0 work. The channel is shared with Legion. |
| M-1 | The tier rule applied only to NEW packages, so 782k EXTEND lines went public by default | D4 now covers the 23 extended packages that only one app adds to (marked `check` in the master plan). App-side licence enforcement is reviewed under D4. |
| M-2 | `rightkit-privileged-helper` was public, contradicting the security mitigation | Now private. |
| M-3 | The development override broke the locked GATE-CONTRACT (GC-T06, GC-T14); an existing local publish route was ignored | Override dropped. The existing `right-release publish … --local` route, batched by layer, is used instead. |
| M-4 | A mandatory P13 amendment puts about 1M lines on 90-day exceptions; proposed "may stay, must move on a second consumer" | **Contested.** It conflicts with Adrian's goal that no app code sits outside the kit. Mitigated instead: the amendment text says product code waiting for its wave is tracked by this plan, not by 90-day exceptions. The narrower alternative is recorded as rejected. |
| M-5 | Wave 0 LEGION moves needed the private channel, which was scheduled for Wave 4 | Building the private channel is now Wave 0, before the LEGION moves. |
| M-6 | APPTEST was only partly applied (journeys and iOS e2e still went into library packages) | Mapper now applies APPTEST to capabilities named e2e, journeys or iOS tests. |
| N-1 | `rightkit-social-mock` as a package was YAGNI | It stays in PostRight as APPTEST; 85 new packages. |
| N-2 | Mesh rename detail was too low-level for this document | Cut to one line; the detail moves to its design doc. |
| N-3 | Tiers were assigned by family only | D4 requires checking tiers against import edges. |
