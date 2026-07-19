# rightkit-db

Correct-by-default SQLite connection bootstrap for Right Suite desktop apps.
It owns connection pragmas, SQLCipher key ordering, bounded read pooling,
checkpointing, optimization, and optional integrity checks. It does not own app
schemas, migrations, or queries.

Plain SQLite is the default feature. SQLCipher consumers must disable defaults:

```toml
rightkit-db = { version = "=0.1.0", default-features = false, features = ["sqlcipher"] }
```

Choose `DurabilityProfile::Full` for user mutations that cannot be reconstructed
and `DurabilityProfile::Normal` for rebuildable indexes or telemetry. The
`LargeWorkingSet` cache profile is opt-in and currently intended for MailRight.

Publish only through:

```text
right-release publish cargo --crate rightkit-db
```
