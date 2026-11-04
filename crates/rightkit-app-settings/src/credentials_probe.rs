//! `SecretProbe` for `rightkit_credentials::Store` (feature `credentials-probe`).

use rightkit_credentials::{Namespace, Store};

use crate::{Presence, SecretProbe, SecretRef};

impl SecretProbe for Store {
    fn presence(&self, secret: &SecretRef) -> Presence {
        let Ok(ns) = Namespace::new(secret.service.clone()) else {
            return Presence::Unknown;
        };
        match self.with_namespace(ns).exists(&secret.account) {
            Ok(true) => Presence::Present,
            Ok(false) => Presence::Absent,
            Err(_) => Presence::Unknown,
        }
    }
}
