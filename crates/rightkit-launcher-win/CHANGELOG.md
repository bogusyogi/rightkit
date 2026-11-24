# Changelog

## 0.2.0

- Breaking: the launcher panel and the handover overlay (`panel`, `spot_takeover`, the `qa-views` feature) move out of the package, with the dependencies they needed (`rightkit-overlay`, `rightkit-overlay-win`, `rightkit-win-ui`, `rightkit-child-process`). Store discovery (`store`), snippet expansion (`snippets`), launcher chat (`chat`) and type-ahead (`type_ahead`) stay; the application draws its own launcher layout, theme and copy.
- Snippet expansion uses the clipboard crate's guarded write sequence for its restore (no separate sample after write-back); requires `rightkit-clipboard-history` 0.4.0 (was `>=0.3.2, <0.5`) so capture, paste and the store resolve to one instance.

## 0.1.0

- First release of Windows launcher services and the original launcher panel.
