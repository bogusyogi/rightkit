# Swift skeptic review — 2026-10-11

Independent static control-flow review of F1–F4 in `swift-review.md`. No build, test, network, install, or source edit was run; prior audit reports were not used. Review is bounded to cited Swift paths and does not claim whole-repository coverage.

F1 is confirmed in narrowed form. `ParakeetEngine` declares `@unchecked Sendable` at `SpeechEngines.swift:61`, exposes mutable `vocabulary` at `:88`, and reads it from async and synchronous decode paths at `:90-113`. A concurrent caller mutation can race those reads, so Sendable safety is not established. `Task.detached` at `:93-103` shares one runtime, while `ParakeetCoreMLStage.predict` calls the same `MLModel` objects at `ParakeetCoreMLStage.swift:117-131`; concurrent Core ML safety is a separate unqualified risk, not needed to confirm the public Array race.

F2 is confirmed. `stop()` increments generation, cancels `startTask`, then calls `teardown()` (`LiveDictationSession.swift:77-81`). `beginLegacy` installs a tap, synchronously starts the engine, creates recognition task, then publishes resources under lock (`:144-166`). There is no generation check between `engine.start()`/`recognitionTask` and publication. A stop interleaving therefore tears down nil state, after which stale engine/request/task are stored and remain active; callback generation checks suppress output but do not release resources.

F3 is confirmed as an App Group design gap. `FileSyncStateStore` explicitly supports an app-group URL (`SyncStateStore.swift:73-75`), but `load`, corrupt quarantine, and `save` (`:80-112`) have no shared file lock. `CloudSyncEngine` owns state only per actor (`CloudSyncEngine.swift:48-112`) and persists whole snapshots (`:693`), so separate app/extension processes can perform load-modify-save interleavings that lose one writer’s token, version, or tombstone. Atomic replace protects byte integrity, not merge integrity.

F4 is confirmed, with impact kept medium. `RightKitWatcher` uses `.unbounded` buffering (`RightKitWatch.swift:181`) and yields from every callback record (`:261-269`). The stream’s termination hook stops only on cancellation/finish; a retained watcher with a stalled consumer can accumulate events without a cap. Existing `mustRescan` flags describe FSEvents loss but do not bound this Swift-side queue.

The packet covered 29 of 1,996 denominator paths and omitted the remainder under its byte cap. Result therefore sets `complete: false`; findings are source-anchored and verified only for cited paths.
