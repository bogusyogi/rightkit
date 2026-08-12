# ADR-003 — user & session model

Status: frozen for Phase 0

Client identity binds an authenticated interactive user/session to a broker request & receipt. Untrusted macOS work uses one non-login runner UID per live request, drops groups before execution & rotates generation after teardown. Windows uses service identity plus authenticated named-pipe clients. Managed roots remain inaccessible to users & runner processes except explicitly granted stage/target/temp roots.
