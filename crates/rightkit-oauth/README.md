# rightkit-oauth

Native OAuth 2.0 authorization-code flow for Right Suite desktop apps. The crate binds a loopback callback on `127.0.0.1:0`, uses S256 PKCE and state validation, supports Google and generic provider endpoints, and retains rotated refresh tokens. Token persistence belongs to an app implementation of `TokenStore`.
