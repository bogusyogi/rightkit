# rightkit-launcher

The portable command launcher core: an arithmetic calculator and unit conversion that never hand typed text to an evaluator (`calc`), user-made items, quicklinks and snippets (`config`), word lookup (`dictionary`), currency and coin rates (`rates`), notes (`notes`), custom shell commands (`commands`), the file search (`files`) and bounded JSON reading (`json`, also used by `rightkit-clipboard-history`). The application installs a `host::Host` for translation, HTTPS and logging. No window code: any platform's launcher UI can sit on it.

The Windows panel that draws these (the layered search window, the in-launcher assistant, snippet expansion, Store apps) is `rightkit-launcher-win`. The assistant's answer layout is `rightkit_overlay_win::response_layout`.

Moved into RightKit from `pulse-windows` (Pulse) on 2026-11-01; split into this core and `rightkit-launcher-win` in 0.2.0 (2026-11-11). The Mac launcher (Pulse's Swift `Launcher`) is not here: whether it calls this core over FFI or stays a Swift module is open.
