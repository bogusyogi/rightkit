# Changelog

## 0.2.0

- **`in-app` feature (off by default).** `rightkit-service` and `serde_json` are now optional behind `in-app`. It gates `in_app::ServiceBridge` and the `paths` module (`evidence_dir`, `per_run_evidence_dir`). The `in_app::InApp` trait and `Automation::add_in_app` stay available without it, so a caller can supply its own channel. Consumers of `ServiceBridge` or `paths` add `features = ["in-app"]`; no workspace crate used either (`rightkit-agent-tools` uses neither).
- Fix the stale "zero third-party dependencies" manifest comment.
