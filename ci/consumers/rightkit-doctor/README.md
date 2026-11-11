# rightkit-doctor 0.2.0 registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-doctor =0.2.0` exactly with its default features. It installs the crate from the sealed archive through the offline directory registry and runs `src/main.rs`, which prints `ok:` lines and exits 1 on the first failed check.

## What it proves

- `verdict`: the worst status wins and sets the exit code; the text report names the fix.
- `row::readiness`: only required rows decide readiness.
- `command::run`: a `DoctorHost` produces the engine report envelope.
- `clock::rfc3339_millis` formats the Unix epoch.

## What it deliberately does not prove

Anything that needs external assets (tools on `PATH`, a clipboard, a desktop session).
