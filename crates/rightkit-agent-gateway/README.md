# rightkit-agent-gateway

Remote tools for agents on this computer: files, documents, terminals, processes, screen, app launch, media, clipboard, chat read and start, saved logins and payments; folder confinement, per-chat unlock, local approvals, owner settings and the activity trail.

Moved into RightKit from Pulse (pulse-core remote/*) on 2026-11-01.

## What the host app supplies

* `rightkit_app_host::install(...)` and `rightkit_chat_bridge::chat::brand::set_brand(...)` once at start (see those crates).
* `rightkit_agent_gateway::config::configure(GatewayConfig { ... })` once at start: the resident engine's discovery name and program, the label a command-line process says hello as, and the host app's own application ids, which are never controlled for an agent.
* `rightkit_agent_gateway::init()` (also run by `execute`) connects the gateway's PIN and unlock state to `rightkit-approvals`; on macOS `rightkit_approvals::set_signing_team(...)` names the team whose signed code may create protected keychain items; on Windows `rightkit_approvals::set_owner_key_service(...)` installs the elevated owner-key service.

Stored-data and signature domains such as `pulse.agent-secrets`, `pulse.protected-settings`, `pulse.chat-unlock` and the `pulse.*.v1` message domains keep their names: renaming them would orphan saved logins, cards, settings and issued unlock tokens.
