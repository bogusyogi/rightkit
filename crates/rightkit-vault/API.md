# rightkit-vault API

```rust
Vault::new(Config::new(app, subject), Arc<dyn ServerKeySource>, Arc<dyn ServerKeyCache>,
           Arc<dyn ClientSecretStore>, Arc<dyn BlobStore>)
vault.load(&token_fn)   -> Result<Option<SecretMap>>  // cache first; works offline within grace
vault.save(&token_fn, &SecretMap) -> Result<()>       // fresh DEK + salts; refuses to replace a blob it cannot open
vault.rotate(&token_fn) -> Result<bool>               // re-wrap the DEK under the current server key
vault.reset(&token_fn)  -> Result<()>                 // DESTRUCTIVE: re-init on the service, fresh empty vault
vault.refresh(&token_fn)-> Result<()>                 // force service confirmation (detects lapse/erase now)
vault.sign_out()        -> Result<()>                 // drop the cached server key
```

`token_fn: &dyn Fn() -> Result<String>` returns a Google ID token; called only on service contact.
`SecretMap = BTreeMap<String, rightkit_credentials::Secret>`.

Traits: `ServerKeySource { fetch, reset }`, `ServerKeyCache { load, store, clear }`,
`ClientSecretStore { get, get_or_create }`, `BlobStore { read, write }`.

Provided: `HttpServerKeySource` (rightkit-http), `CredentialsKeyCache` and
`CredentialsClientSecret` (rightkit-credentials; the latter also has `install_if_absent` and
`delete`). The caller supplies the `BlobStore`.

Errors: `Unauthorized` (401), `Denied` (403, not entitled), `Revoked` (410, erased), `Offline`,
`Service`, `Store`, `Cache`, `ClientSecret`, `ClientSecretMissing`, `Decrypt`, `KeyUnavailable`,
`Format`, `Version`, `Entropy`, `Crypto`.
