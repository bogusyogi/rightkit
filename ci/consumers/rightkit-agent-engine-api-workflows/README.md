# rightkit-agent-engine-api-workflows registry consumer

Standalone package that depends on `rightkit-agent-engine-api-workflows` by exact registry pin (`=0.2.0`) outside the RightKit workspace and
exercises a few cheap public API checks with no external assets. Each passing check prints an `ok:` line; a failure exits 1.
