# rightkit-logs 0.3.4 historical qualification

0.3.4 is an unpublished candidate. Its immutable source commit is [`3f8116ae5117a490a683bd9a99b4f63ae183c310`](https://github.com/bogusyogi/rightkit/commit/3f8116ae5117a490a683bd9a99b4f63ae183c310). The preserved original `runtime-budget.json` has SHA-256 `10465b7ed340ea2851bfb311360850367ead1966e5e82f7edaa50e113e8ed5cc`. The source candidate also produced artifact SHA-256 `c37d3ed6ae2d2c3199767cd07c152706a3bc8242647f3f30c3290257535f2855`; it is retained as historical evidence and is not publishable.

Its Windows tracing measurement reports 1.394 wake-ups/second against the 1/second ceiling. This is an unresolved overage; no runtime fix is claimed. The detached 1000 ms workload failure is separate evidence and does not qualify the selected 2000 ms workload. Original raw budget evidence is copied byte-for-byte in [`runtime-budget.evidence/`](runtime-budget.evidence/).
