# Changelog

## 0.3.2

- New default-on `async` feature gates the `subscribe` module (`FileWatcher`, `FileWatcherSubscriber`,
  `FileWatcherEvent`, `Receiver`, `DebouncedWatchReceiver`, `ThrottledWatchReceiver`, `WatchPath`,
  `WatchRegistration`) and the `tokio` dependency. Default builds are unchanged;
  `default-features = false` compiles no async runtime.
- New `TypedWatcher::watch(path)`: the runtime-free constructor for the typed recursive stream.
  Previously the only way to build a `TypedWatcher` was `FileWatcher::watch_typed`, which now sits
  behind `async`; that method still exists and delegates to `watch`.
