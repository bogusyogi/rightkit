# Changelog

## 0.1.2 (unpublished)

- One home for the device reference, effect classes and principals: the `device`, `effect` and `principal` modules
  are `rightkit-agent-task-contract` 0.1.0's, re-exported under the same paths (`rightkit_device_link_types::device::DeviceRef`,
  `rightkit_device_link_types::EffectClass`, ...). The copies here (line for line the task contract's, apart from
  comments) are deleted. Same names, fields, wire format and schema-version constants. Decision 4 of the
  2026-11-11 shape review (docs/audits/2026-11-11-shape-matrix/README.md, "Agent engine") taken on the recommended
  option, pending the owner's confirmation; design note:
  docs/audits/2026-11-11-shape-matrix/reports/agent-engine-contract-design.md.

## 0.1.1 (unpublished)

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from Pulse (pulse-core agent and devicelink).
