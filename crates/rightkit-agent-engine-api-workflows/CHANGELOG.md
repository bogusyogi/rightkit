# Changelog

## 0.2.0

- Breaking: `CreateOptimizationRequest` carries `rightkit_agent_learning::WorkflowProposalKind` and `rightkit_agent_learning::EvalEvidence` (`rightkit-agent-learning` 0.1.0), the same types under the crate's new name: CodeRight's learning crate was published as `rightkit-adapt` 0.1.x and is renamed, because `rightkit-adapt` 0.2.0 is now Pulse's governed learning. Taken on the recommended option of the 2026-11-11 shape review, pending the owner's confirmation.

## 0.1.1 (unpublished)

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from `coderight-api-workflows` (engine).
