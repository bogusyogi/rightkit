// rightkit-passkeys 0.1.0 consumed by exact registry pin (`=0.1.0`), outside the workspace.
// Implements the crate's two consumer traits over in-memory fakes of a vault and a user prompt, then
// exercises the pure relying-party normalisation and the adapters' constructors. No authenticator
// ceremony runs, no key is created, and no file, network or credential store is touched.
use std::future::Future;
use std::task::{Context, Poll, Waker};

use async_trait::async_trait;
use rightkit_passkeys::{
    normalize_rp_id, CredentialStore, Passkey, StoreAdapter, StoreError, UserOutcome, UserRequest,
    UserVerifier, VerifierAdapter,
};

fn check(name: &str, ok: bool) {
    if !ok {
        eprintln!("FAIL: {name}");
        std::process::exit(1);
    }
    println!("ok: {name}");
}

// A vault with no passkeys in it.
struct EmptyVault;

impl CredentialStore for EmptyVault {
    fn find(
        &self,
        _credential_ids: Option<&[Vec<u8>]>,
        rp_id: &str,
        _user_handle: Option<&[u8]>,
    ) -> Result<Vec<Passkey>, StoreError> {
        if rp_id.is_empty() {
            return Err(StoreError("empty relying party".into()));
        }
        Ok(Vec::new())
    }

    fn save(&mut self, _passkey: Passkey) -> Result<(), StoreError> {
        Ok(())
    }

    fn update_counter(&mut self, _credential_id: &[u8], _counter: u32) -> Result<(), StoreError> {
        Ok(())
    }
}

// A prompt that reports exactly what it was asked for and can verify the user only when told to.
struct ScriptedPrompt {
    can_verify: bool,
}

#[async_trait]
impl UserVerifier for ScriptedPrompt {
    fn supports_verification(&self) -> bool {
        self.can_verify
    }

    async fn confirm(
        &self,
        request: UserRequest,
        require_presence: bool,
        require_verification: bool,
    ) -> Result<UserOutcome, StoreError> {
        match request {
            UserRequest::InformNoCredentials => Err(StoreError("declined".into())),
            _ => Ok(UserOutcome {
                presence: require_presence,
                verification: require_verification && self.can_verify,
            }),
        }
    }
}

// The futures here complete on the first poll, so a no-op waker is enough.
fn block_on<F: Future>(future: F) -> F::Output {
    let mut future = Box::pin(future);
    let mut context = Context::from_waker(Waker::noop());
    loop {
        if let Poll::Ready(value) = future.as_mut().poll(&mut context) {
            return value;
        }
        std::thread::yield_now();
    }
}

fn main() {
    // Relying-party id normalisation.
    check(
        "case and trailing dot",
        normalize_rp_id("Example.COM.").ok().as_deref() == Some("example.com"),
    );
    check(
        "surrounding whitespace is trimmed",
        normalize_rp_id("  a-b_c.d  ").ok().as_deref() == Some("a-b_c.d"),
    );
    check(
        "longest id accepted",
        normalize_rp_id(&"a".repeat(253)).is_ok(),
    );
    for (name, raw) in [
        ("empty", String::new()),
        ("bare dot", ".".to_string()),
        ("leading dot", ".example.com".to_string()),
        ("empty label", "a..b".to_string()),
        ("scheme", "https://example.com".to_string()),
        ("port", "example.com:8443".to_string()),
        ("space", "ex ample.com".to_string()),
        ("non-ASCII", "ex\u{e4}mple.com".to_string()),
        ("over-long", "a".repeat(254)),
    ] {
        check(
            &format!("{name} id refused"),
            normalize_rp_id(&raw).is_err(),
        );
    }

    // The store trait through the adapter's accessor.
    let adapter = StoreAdapter::new(EmptyVault);
    check(
        "empty vault finds nothing",
        adapter
            .store()
            .find(None, "example.com", None)
            .map(|found| found.len())
            == Ok(0),
    );
    check(
        "store errors carry their text",
        adapter.store().find(None, "", None).err()
            == Some(StoreError("empty relying party".into()))
            && StoreError("boom".into()).to_string() == "boom",
    );
    let _verifier_adapter = VerifierAdapter::new(ScriptedPrompt { can_verify: false });

    // The verifier trait: what the prompt reports is what the caller gets.
    let prompt = ScriptedPrompt { can_verify: true };
    check("verifier capability", prompt.supports_verification());
    let create = UserRequest::Create {
        rp_id: "example.com".into(),
        user_name: Some("ada".into()),
    };
    check(
        "presence and verification are reported truthfully",
        block_on(prompt.confirm(create.clone(), true, true))
            == Ok(UserOutcome {
                presence: true,
                verification: true,
            }),
    );
    let weak = ScriptedPrompt { can_verify: false };
    check(
        "an unverifying prompt does not claim verification",
        block_on(weak.confirm(create, true, true))
            == Ok(UserOutcome {
                presence: true,
                verification: false,
            }),
    );
    check(
        "a declined prompt is an error",
        block_on(prompt.confirm(UserRequest::InformNoCredentials, false, false))
            == Err(StoreError("declined".into())),
    );
    check(
        "request values compare",
        UserRequest::InformExcluded {
            rp_id: "a.com".into(),
        } != UserRequest::Use {
            rp_id: "a.com".into(),
            user_name: None,
        },
    );

    println!("OK rightkit-passkeys 0.1.0 (registry consumer, public API)");
}
