# Changelog

## 0.1.0

- First release: the Win32 panel of `rightkit-launcher` 0.1.0 (`panel`, `chat`, `snippets`, `spot_takeover`, `store`, feature `qa-views`), moved with `git mv`, over the portable core `rightkit-launcher` 0.2.0 and `rightkit_overlay_win::response_layout`.
- Its own host, `host::PanelHost`, for the panel's services (the windows-only half of the old `rightkit_launcher::host::Host`).
- Pulse's product out of the code: the "Pulse" section is the host application's section (`app_section_title`, section name `app`; the configuration name `pulse` is still read); Pulse's seven hub pages are the host's `app_pages`; the Claude and Codex usage rows are the host's `usage_rows` (`usage_line` is gone); the storage and cleanup rows appear only when the host names their pages (`storage_pages`) and the cleanup row reads the host's `cleanup_summary` instead of Pulse's `cleanup-findings-v1.json`; the application commands switch and the stored Alt+Space answer are named by the host (`PanelHost::app_commands_switch`, default `launcherAppCommands`; `PanelHost::hotkey_conflict_take_token`, default `app`): both are stored names the application's other surfaces share (Pulse's Mac app and hub keep `launcherPulseCommands` and accept only `pulse` or `other`), so the panel reads exactly the one switch the host names, and writes the value the host's settings accept; messages name the host's `product_name` or no product.
- Taken on the recommended option of the 2026-11-11 shape review, pending the owner's confirmation.
