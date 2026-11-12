# rightkit-launcher-win

The Windows launcher panel over the portable core `rightkit-launcher`: a layered search window opened by Alt+Space (or Ctrl+Space) that lists pinned, frequent and running apps, Start menu and Store apps, files, notes, quicklinks, snippets, custom commands, the clipboard history, word lookup, the in-launcher assistant and web search, with calculator answers first. Snippet expansion and the owner-side handover overlay are here too. Windows only; empty elsewhere.

The application installs `host::PanelHost` (and the core's `rightkit_launcher::host::Host`). Nothing in the crate names a product: the product name in the Alt+Space question, the heading of the application's section, its pages (`app_pages`), its readings (`usage_rows`), its storage and cleanup pages and its saved cleanup scan (`storage_pages`, `cleanup_summary`) and the data folder (`data_dir`) are the host's. Settings are read through `switch_on` (`launcherEnabled`, `launcherAppCommands` (or its earlier name `launcherPulseCommands`), `launcherClipboard`, `launcherSnippetExpansion` and others).

Moved into RightKit from `pulse-windows` (Pulse) on 2026-11-01 inside `rightkit-launcher` 0.1.0; split out on 2026-11-11, with Pulse's pages, readings, cleanup file and strings moved to the host.
