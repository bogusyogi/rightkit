# SwiftRight

Fresh source audit at `4103ff3064382839052d64e78c1b05be942d2e20`; scope covers `SwiftRightBridge`, `SwiftRightServices`, Windows renderer, & qualification runner/config. Examples, probes, conformance traces, generated output, caches, & build artifacts excluded; no builds/install/network.

Status: **0 Y, 0 N, 0 M, 97 NA**.

Unsupported reuse opportunities, outside current cutover scope:

- `cargo:rightkit-framed-sidecar` — Swift `EngineWire` implements compatible framing, but owner exports Rust generic APIs only. Establish a supported Swift/C/native-host bridge before migration review.
- `cargo:rightkit-process` — `EngineClient` uses Foundation process APIs, while owner exports Rust lifecycle types only. Establish a supported Swift/C/native-host bridge before migration review.
- `cargo:rightkit-qa` — `qualification/run.py` orchestrates Python qualification stages, while owner exposes Rust library/CLI targets only. Establish a supported Python/C/native-host bridge before migration review.

Adversarial review challenged renderer text cache vs disk cache, modal menus vs overlay policy, frame trace vs structured logs, & native SwiftRight bridge/renderer vs RightKit Apple UI. Three apparent N rows became NA: each local caller targets a Rust-only owner with no supported bridge.

