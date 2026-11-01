# rightkit-voice-commands registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-voice-commands` to the exact version
in its `Cargo.toml`. It proves, through the public API only: a direct phrase ("select all")
recognizes as a key sequence, ordinary dictation does not, the command catalog is non-empty, and
the external link allowlist accepts exactly the listed forms.

State: in-memory values only. No network, no files, no process spawn, no OS side effects. Run only
by the CI consumer lane.
