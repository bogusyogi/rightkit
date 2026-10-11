# Publication skeptic review — 2026-10-11

Independent review of fresh publication and runtime-budget evidence against live release source. No old audit, test, build, network, install, credential read, publication, or product edit was used.

F1 is confirmed. Current `HEAD` is `f60f116514467e83952f5e5c12a71b34e3341389`; all 60 local evidence files name earlier commits, so zero match current `HEAD`. `publication-gate.mjs:34-48` rejects dirty package paths and exact-commit mismatch; `:55-68` also requires host-run records and a 64-hex artifact hash. `publication-review.md:100` correctly reports zero current public-unit receipts. This is a release-readiness block, not evidence that earlier releases were invalid.

F2 is confirmed with scope narrowed to gate coverage. `RUNTIME-BUDGETS.md:7-13` requires measured resource and installed-app proof. `runtime-budgets.mjs:88-109` only parses README text for resident, CPU, and one measured line; `publish-cargo.mjs:118-125` rejects only that static failure during real cargo publication. All 11 listed README files currently lack `## Runtime budget`. `rightkit-daemon` and `rightkit-send` set `publish = false` (`Cargo.toml:9`, `Cargo.toml:10`), so cargo publication does not qualify their app adoption. `rightkit-process` owns a resident watchdog (`stall.rs:124-144`) and `rightkit-models` owns a model lease held while loaded (`lease.rs:1-10`, `:31-45`); neither appears in required membership. Their addition remains a policy decision, while missing coverage is factual.

F3 is confirmed as unqualified portability, not package breakage. `publication-review.md:102` records no archive or fresh install. Cargo plan has package-content and registry-dry-run stages (`publish-cargo.mjs:54-67`), but this bounded review did not execute them and has no equivalent current npm/Swift clean-install receipt. No standalone-install claim is justified.

Packet status remains incomplete. Fresh evidence is source-anchored, but no trusted host receipt was created or ingested; `reasoning.release-readiness.json` therefore keeps `complete: false`.
