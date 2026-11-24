# rightkit-launcher-win

Windows launcher services over the portable `rightkit-launcher` core. Use `store` for Store app discovery, `snippets` for snippet expansion, `type_ahead` to hold keys while a launcher opens and `chat` for launcher conversations. The crate is empty on other platforms.

Install `host::PanelHost` for snippet settings, input and launcher chat requests. Install `rightkit_launcher::host::Host` for translation and logging. The application supplies its launcher layout, theme and copy.
