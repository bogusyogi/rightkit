# Membrane — retained

Selected /Volumes/D/worktrees/membrane-live-diagnostics-phase1-repair as source root: 1,039 operative source files versus 1,025 in membrane-main-f602-baseline & 1,039 in membrane-blueprint-findings. All three are retained snapshots; none is counted as concurrent implementation or primary active repository.

97/97 catalog rows reviewed: 5 Y, 5 N, 0 M, 87 NA.

Y: @rightkit/ax, @rightkit/git, @rightkit/hooks, @rightkit/platform-ui, @rightkit/release.

N: cargo:rightkit-db (Cortex direct SQLite), cargo:rightkit-embed (Cortex FastEmbedder), cargo:rightkit-mcp (Membrane stdio MCP server), @rightkit/shell (Hub raw renderer bridge), @rightkit/tauri (Hub raw host commands). Compare exact APIs before cutover; preserve Cortex schemas, Membrane protocol/tool policy, & Hub commands.

No M: no exact overlap has both local implementation & live shared production route.
