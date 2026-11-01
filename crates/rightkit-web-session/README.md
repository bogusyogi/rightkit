# rightkit-web-session

Agent browser logins: one owner-approved Chromium session at a time on the login profile, with a network policy limited to the approved sites, a live view for the owner and signed approvals.

Moved into RightKit from Pulse (pulse-core browser/*) on 2026-11-01.

## Features and host setup

* `browser` (off by default) turns on Chromium sessions through `rightkit-browser` on a Mac or Linux computer; without it every call answers `unsupported`.
* The host installs `rightkit_app_host` (the state folder) and `rightkit_approvals::set_admin_key_file` / `set_hooks` through `rightkit_agent_gateway::init()`; on a headless Linux computer `set_linux_settings_file` names the root-owned file that switches the feature on.
