# rightkit-agent-tools 0.3.0 registry consumer (PK15)

Standalone consumer, outside the workspace, pinning `rightkit-agent-tools =0.3.0` exactly with default
features off. It exercises `ApplicabilityRecord::from_manifest`, the M22 record in
`rightkit-mandatory.json`: client and absent roles, the optional key, and the refusals.

State: in-memory strings only. No files, no network, no process spawn, no user data. Run only by the
CI consumer lane.

Not covered: the toolbox, backends, journal, exposure rules, and the control toolbox.
