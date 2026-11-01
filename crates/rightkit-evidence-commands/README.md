# rightkit-evidence-commands

Evidence, freeze, contract, decision, governance, key, state and run command logic as library modules (`commands::*`): argument parsing and handlers that return JSON or a typed `CommandError`. The host CLI only registers commands and prints results. Host names (program, workspace directory, config file, authority store) are declared with `commands::set_command_host`.

Moved into RightKit from the Legion CLI (`engine/bins/legion/src/commands`) on 2026-11-01.
