# RustRight RightKit cutover matrix

Reviewed 2026-10-11T07:59:59+05:30 at `94f01de9481619dacd62f04872360b183ee5bd59`. Targets: macOS & Windows broker/CLI, installer & parity tool.

Counts: **3 Y / 4 N / 0 M / 90 NA** across 97 rows.

Shared production routes: `rightkit-fs` atomic/private writes & durable sync, `rightkit-process` contained child lifecycle, `rightkit-ownership` admission scanner.

Concrete gaps: `rightkit-logs` has local ThinLogger JSONL lifecycle logs; `rightkit-jobs` has local broker job operations; `rightkit-service` has custom local socket/pipe host; `rightkit-sandbox` has local pre-spawn capacity reservation. Preserve build-control protocol, workspace exclusion & policy when evaluating adapters.

All npm UI/tooling rows lack a JavaScript runtime. All Swift rows lack Swift/Apple app targets. Source-only audit; no execution qualification.
