# rightkit-agent-tools 0.3.0 registry consumer (PK15)

Standalone consumer, outside the workspace, pinning `rightkit-agent-tools =0.3.0` exactly with default
features off (the optional `automation`/`browser`/`speech` backends stay out, so unpublished
`rightkit-automation` 0.6.0 is never needed to run it; `rightkit-daemon` 0.4.0 remains the hard
dependency). It exercises `ApplicabilityRecord::from_manifest`, the M22 record in
`rightkit-mandatory.json` — client and absent roles, the optional key, and the refusals — and hosted
admission through the public `Toolbox` gate: a denial from a host `ToolAdmission` hook executes
nothing and journals `Denied{by: Admission}`, an unlisted caller is refused first, and a paste-named
tool cannot register.

State: in-memory values only. No files, no network, no process spawn, no user data. Run only by the
CI consumer lane.

Not covered: the backends and the control toolbox.
