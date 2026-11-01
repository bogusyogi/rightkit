# rightkit-privileged-helper

Elevation broker for root-owned uninstall moves. A background process parks the root-owned paths of an owner-started uninstall; the trusted UI that may talk to the privileged helper polls `next`, asks the owner, calls the helper and answers with `complete`. Waits are bounded and nothing is parked unless the UI is polling.

Moved into RightKit from the Pulse engine on 2026-11-01. The Swift helper and its XPC client are separate packages.
