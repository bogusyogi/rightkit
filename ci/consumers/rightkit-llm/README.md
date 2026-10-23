# rightkit-llm 0.3.1 registry consumer (PK26)

Standalone consumer, outside the workspace, pinning `rightkit-llm =0.3.1` exactly with default features. It proves the
attempt-billing contract through the public API: connect-phase failures and unbuildable requests are `Spend::NotSpent`,
dispatched failures are `Spend::Unknown`, 4xx (not 408) is `NotSpent` and 5xx is `Unknown`. In-memory only.
