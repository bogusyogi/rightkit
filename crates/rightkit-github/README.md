# rightkit-github

GitHub App authority contracts, forge request specs, a file-backed App state store, webhook
verification, installation-token lifecycle, a `github` CLI command model, and desktop/CLI device-flow
sign-in for Right Suite apps.

Moved into RightKit from the CodeRight daemon (`github_app`, `github_signin`) on 2026-11-01.

Hosts call `signin::configure` with their GitHub client id and credential namespace before sign-in,
and pass their state root to `daemon::github_app_state_path`.
