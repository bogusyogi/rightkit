# macOS Phase-0 harness

`pnpm probe` produces current, non-enforcing evidence. It never installs a system extension, changes session lock state, creates runner accounts, grants ACLs, or activates deny policy. `extension/` is compile-oriented scaffold only: entitlement grant, signed universal host app, `/Applications` activation, interactive approval, Full Disk Access, load/stall/upgrade testing remain stop/go work.

Status stays `BLOCKED_EXTERNAL` until Apple grants `com.apple.developer.endpoint-security.client`; Developer ID signing identity does not grant that entitlement. If Apple account sign-in is unavailable, evidence records `UNKNOWN_NOT_AUTHENTICATED`, never a pending or denied disposition.
