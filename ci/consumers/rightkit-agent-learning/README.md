# rightkit-agent-learning registry consumer

Standalone package that depends on `rightkit-agent-learning` by exact registry pin (`=0.1.0`) outside the RightKit workspace and
exercises a few cheap public API checks with no external assets: weak evidence does not pass the evaluation gate, and the
proposal store over an in-memory agent database starts with no pending proposal. Each passing check prints an `ok:` line; a
failure exits 1. (Until 0.1.2 this crate was published as `rightkit-adapt`; that name now holds Pulse's governed learning.)
