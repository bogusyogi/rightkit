# rightkit-code-audit-commands

Code audit command logic as library modules (`commands::*`): audit, audit assurance and runtime, coverage, minimize, report, review, rules and verify. Handlers return JSON or a typed `CommandError` (shared with `rightkit-evidence-commands`); the host CLI only registers commands and prints results. The host declares its installed share directory with `commands::set_installed_share_dir`.

Moved into RightKit from the Legion CLI (`engine/bins/legion/src/commands`) on 2026-11-01.
