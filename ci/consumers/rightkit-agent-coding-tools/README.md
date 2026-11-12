# rightkit-agent-coding-tools registry consumer

Standalone package that depends on `rightkit-agent-coding-tools` by exact registry pin (`=0.1.0`) outside the RightKit workspace
and exercises a few cheap public API checks with no external assets, through the edit helper every file-editing tool shares: an
exact match is replaced once, an ambiguous match is refused instead of guessed, `replace_all` replaces every match, and a missing
match is an error. Each passing check prints an `ok:` line; a failure exits 1. (Up to `rightkit-agent-tools` 0.3.4 this code was
that crate's `coding_tools` module.)
