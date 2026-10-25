# rightkit-workflow registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-workflow` to the exact version in its
`Cargo.toml` with its one public feature, `sequence`. Features only add API, so everything the
default build offers is proven here too. It also depends on `serde_json = "1"` and `blake3 = "1"`,
both already in the crate's own dependency closure, to check journal records and bundle digests. It
is built from the sealed archive through the offline directory registry. Nothing in it is
platform-gated, so Windows, macOS and Linux run the same assertions.

Time is a `ManualClock`, journals live in a per-process temp directory, and the remote worker wire
runs over an in-memory byte pipe, so there is no socket. It proves:

- Model: `StepSpec` and `Scenario` wire names and `validate`, `IdempotencyKey`, `StepRef` ordering
  and display, `Phase::is_terminal`, `ManualClock`, `SystemClock`, `CancelToken`.
- `Run`: `perform` runs at most once per run, step and take, with `Dispatched` durable first;
  observe is polled on the clock until satisfied, failed, cancelled or the deadline (`Unresolved`);
  a transient observe error and a perform error both end in observe; leases carry fencing tokens
  (`Held`, `renew`, expiry, a superseded lease is `Fenced`, a terminal step cannot be leased); a
  lease that expires mid-step leaves the step `Performed`, and a restart from the journal resumes
  into observe without performing again; `retake` replays the prefix into a new take and flags a
  differing postcondition and the steps it invalidates.
- `Journal`: one JSON object per line with contiguous sequence numbers, a torn tail truncated on
  open, a wrong run, a corrupt line and a sequence gap refused, and the `Event` wire shape.
- `sequence`: `SequenceRun` advances one step per call, reports `Done` again without performing,
  resumes after a restart, stops at a `Gate` before anything is journalled, reports failed,
  cancelled and unresolved steps, and `ReplayPolicy::ReissueOnce` allows exactly one further take.
- The worker wire: frame limits and hex helpers, a sealed `Channel` that refuses tampering, replay,
  reordering and reflection; enrolment with a one-time code (burned on any attempt, label and TTL
  checked); discovery; a step driven by `RemoteDriver` through `Run::execute` and performed exactly
  once; fencing and perform deduplication across connections; resumable bundle transfer with digest
  and size checks; `reconnect`; `WrongWorker` and `NotConnected`.
- `net`: the loopback-only policy (`is_loopback`, `check_addr`, and the refusal in `bind` and
  `connect`), without opening a socket.

It does not prove real sockets (`bind` and `connect` to a loopback address, `accept`), a GUI driver,
a process kill between the journal write and its fsync, the directory fsync on Unix, or the wire
across a LAN (the crate refuses it unless the caller opts in). The handshake helpers in `wire`
(`auth_key`, `identify_mac`, `accept_mac`, `enrol_proof`, `grant_mac`, `session_key`, `code_key`,
`wrap_pad`, `xor32`) and the message enums in `protocol` are exercised only through the enrolment,
discovery and execute flows above, not asserted one by one.

State: journal files under a per-process temp directory, removed at the end; worker threads serve
the in-memory pipes and are joined. A watchdog thread ends the process with a failure if the run
exceeds 50 seconds. No network, no process spawn, no real user data. Run only by the CI consumer
lane.
