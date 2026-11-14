# rightkit-agent-local-models 0.1.1 registry consumer

Standalone package outside the RightKit workspace. It depends on `rightkit-agent-local-models` by exact registry
pin (`=0.1.1`) with default features. It has no path, Git, workspace or patch dependency, no committed
`Cargo.lock` and no other dependency.

Exercises the pure parts: `model_catalog` over a stated hardware report (a 64 GB host with a 24 GB GPU fits every
candidate; a 2 GB host with no GPU fits none, at high confidence; an unknown memory size is `Unknown`, not a
guess), `provider_route_suggestion`, and the bounded inference seam (`validate_inference_request` refuses an
over-limit context, output budget and wall clock; `infer_local_model` reports `NoLocalModelAvailable`;
`infer_with_backend` reaches a backend only with a request inside the limits).

State: process memory only. No file, no network, no process spawn, no real user data.
`probe_hardware_with_adapter` launches probe processes through a sandbox adapter and is never called.

Proof row: public API surface of the new crate (moved from `rightkit-models` 0.5.9 `agent_local_models`). Each
check prints an `ok:` line; a failed check prints `FAIL:` and exits 1.
