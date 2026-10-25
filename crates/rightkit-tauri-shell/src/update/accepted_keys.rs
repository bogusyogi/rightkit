//! Which updater key checks a release, so the suite can rotate its updater key without stranding
//! an installed app (feature `update-license`).
//!
//! The updater plugin verifies a download with exactly one public key, fixed when the check is
//! built. The suite lists every key an app accepts (`rightkit_license::suite_api::updater_keys`),
//! and a release names the key that signed it in its minisign signature. [`check_across_keys`]
//! therefore asks the update server with the transport's configured key first and, when the
//! release turns out to be signed by another accepted key, asks again with that key, so the plugin
//! verifies the download with the right one. The plugin still does all the cryptography; this
//! module only chooses which of the accepted keys it uses, and refuses a release whose key is not
//! accepted before anything is downloaded.

use rightkit_license::suite_api::{self, UpdaterKey};

use super::TransportError;

/// Check for an update across `keys`.
///
/// `fetch(public_key)` asks the update server with that key and answers the release found (any
/// type `T` that carries its signature), `None` when there is no newer one. `signature_of` reads
/// the minisign signature text out of a release. `now` is unix seconds, `None` when the clock
/// cannot be read (a key with an end date is then refused).
///
/// * No `keys` (a transport configured with one key of its own): the first answer stands, as
///   before.
/// * Otherwise the signature's key id must name a listed key that has not ended
///   ([`TransportError::Verify`] when it is malformed, unknown or ended).
/// * A release signed by the key it was fetched with is returned as it is.
/// * A release signed by another accepted key is fetched again with that key. The second answer is
///   the one used, and only if it names that same key (a server that changes its mind between the
///   two requests is refused, never mixed). If the second answer is "no update", there is none.
pub(super) fn check_across_keys<T>(
    primary: &str,
    keys: &[UpdaterKey],
    now: Option<i64>,
    mut fetch: impl FnMut(&str) -> Result<Option<T>, TransportError>,
    signature_of: impl Fn(&T) -> &str,
) -> Result<Option<T>, TransportError> {
    let Some(found) = fetch(primary)? else {
        return Ok(None);
    };
    if keys.is_empty() {
        return Ok(Some(found));
    }
    let chosen = suite_api::select_updater_key(keys, signature_of(&found), now)
        .map_err(|_| TransportError::Verify)?;
    if chosen.public_key == primary {
        return Ok(Some(found));
    }
    let Some(again) = fetch(chosen.public_key)? else {
        return Ok(None);
    };
    match suite_api::select_updater_key(keys, signature_of(&again), now) {
        Ok(second) if second.public_key == chosen.public_key => Ok(Some(again)),
        _ => Err(TransportError::Verify),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    const KEY_A: &str = "29395F9FF466261D";
    const KEY_B: &str = "0123456789ABCDEF";
    const KEY_C: &str = "FEDCBA9876543210";

    /// Standard padded base64 (the shell has no base64 dependency and the tests need no more than this).
    fn std_b64(bytes: &[u8]) -> String {
        const ALPHABET: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in bytes.chunks(3) {
            let n = chunk
                .iter()
                .enumerate()
                .fold(0u32, |n, (i, b)| n | (u32::from(*b) << (16 - 8 * i)));
            for i in 0..4 {
                out.push(if i <= chunk.len() {
                    ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char
                } else {
                    '='
                });
            }
        }
        out
    }

    fn id_bytes(key_id: &str) -> Vec<u8> {
        (0..8)
            .map(|i| u8::from_str_radix(&key_id[2 * i..2 * i + 2], 16).unwrap())
            .rev()
            .collect()
    }

    /// A release signature (as `latest.json` carries it) made by a made-up key id.
    fn signature(key_id: &str) -> String {
        let mut raw = vec![b'E', b'D'];
        raw.extend(id_bytes(key_id));
        raw.extend([0u8; 64]);
        std_b64(
            format!(
                "untrusted comment: signature from tauri secret key\n{}\ntrusted comment: t\n{}\n",
                std_b64(&raw),
                std_b64(&[0u8; 64])
            )
            .as_bytes(),
        )
    }

    fn key(key_id: &'static str, public_key: &'static str, not_after: Option<i64>) -> UpdaterKey {
        UpdaterKey {
            key_id,
            public_key,
            not_after,
        }
    }

    /// What the fake update server answers, and which key text each answer was fetched with.
    struct Server {
        answers: RefCell<Vec<Option<String>>>,
        asked_with: RefCell<Vec<String>>,
    }

    impl Server {
        fn new(answers: &[Option<&str>]) -> Self {
            Self {
                answers: RefCell::new(answers.iter().map(|a| a.map(str::to_owned)).rev().collect()),
                asked_with: RefCell::new(Vec::new()),
            }
        }

        /// A release is its signature text and the key text it was fetched with.
        fn fetch(&self, public_key: &str) -> Result<Option<(String, String)>, TransportError> {
            self.asked_with.borrow_mut().push(public_key.to_owned());
            let next = self.answers.borrow_mut().pop().expect("no more answers");
            Ok(next.map(|signature| (signature, public_key.to_owned())))
        }
    }

    fn run(
        server: &Server,
        primary: &str,
        keys: &[UpdaterKey],
        now: Option<i64>,
    ) -> Result<Option<(String, String)>, TransportError> {
        check_across_keys(
            primary,
            keys,
            now,
            |public_key| server.fetch(public_key),
            |release| release.0.as_str(),
        )
    }

    /// The rotation window: the old key still accepted (until 2_000), the rotated key accepted.
    fn window() -> Vec<UpdaterKey> {
        vec![key(KEY_A, "key-a", Some(2_000)), key(KEY_B, "key-b", None)]
    }

    #[test]
    fn update_accepts_additional_key() {
        // An old-key release is accepted with one request, by the configured key.
        let server = Server::new(&[Some(&signature(KEY_A))]);
        let release = run(&server, "key-a", &window(), Some(1_000))
            .unwrap()
            .unwrap();
        assert_eq!(release.1, "key-a");
        assert_eq!(server.asked_with.borrow().as_slice(), ["key-a"]);

        // A rotated-key release is fetched again with the rotated key, and that answer is used.
        let server = Server::new(&[Some(&signature(KEY_B)), Some(&signature(KEY_B))]);
        let release = run(&server, "key-a", &window(), Some(1_000))
            .unwrap()
            .unwrap();
        assert_eq!(
            release.1, "key-b",
            "the plugin verifies the download with the rotated key"
        );
        assert_eq!(server.asked_with.borrow().as_slice(), ["key-a", "key-b"]);

        // The same holds from the other side: a transport configured with the rotated key
        // accepts a release the old key signed, while the old key has not ended.
        let server = Server::new(&[Some(&signature(KEY_A)), Some(&signature(KEY_A))]);
        let release = run(&server, "key-b", &window(), Some(1_000))
            .unwrap()
            .unwrap();
        assert_eq!(release.1, "key-a");

        // An unknown key is refused after the first request: nothing else is asked.
        let server = Server::new(&[Some(&signature(KEY_C))]);
        assert_eq!(
            run(&server, "key-a", &window(), Some(1_000)),
            Err(TransportError::Verify)
        );
        assert_eq!(server.asked_with.borrow().len(), 1);
    }

    #[test]
    fn a_key_past_its_end_date_is_refused_and_an_unreadable_clock_never_extends_it() {
        let server = Server::new(&[Some(&signature(KEY_A))]);
        assert_eq!(
            run(&server, "key-b", &window(), Some(2_000)),
            Err(TransportError::Verify),
            "the old key ended at 2_000"
        );
        let server = Server::new(&[Some(&signature(KEY_A))]);
        assert_eq!(
            run(&server, "key-b", &window(), None),
            Err(TransportError::Verify),
            "no clock, no key that has an end date"
        );
        // The rotated key has no end date and needs no clock.
        let server = Server::new(&[Some(&signature(KEY_B))]);
        assert!(run(&server, "key-b", &window(), None).unwrap().is_some());
    }

    #[test]
    fn no_update_is_no_update_whichever_request_says_so() {
        let server = Server::new(&[None]);
        assert_eq!(run(&server, "key-a", &window(), Some(0)), Ok(None));
        assert_eq!(server.asked_with.borrow().len(), 1);
        // The second request answers "none": there is none, and nothing is returned from the first.
        let server = Server::new(&[Some(&signature(KEY_B)), None]);
        assert_eq!(run(&server, "key-a", &window(), Some(0)), Ok(None));
    }

    #[test]
    fn a_server_that_changes_its_answer_between_the_two_requests_is_refused() {
        // The second answer is signed by the key the first one was, not the key it was asked with.
        let server = Server::new(&[Some(&signature(KEY_B)), Some(&signature(KEY_A))]);
        assert_eq!(
            run(&server, "key-a", &window(), Some(0)),
            Err(TransportError::Verify)
        );
        // Or by a key that is not listed at all.
        let server = Server::new(&[Some(&signature(KEY_B)), Some(&signature(KEY_C))]);
        assert_eq!(
            run(&server, "key-a", &window(), Some(0)),
            Err(TransportError::Verify)
        );
        // Or is not a signature.
        let server = Server::new(&[Some(&signature(KEY_B)), Some("not a signature")]);
        assert_eq!(
            run(&server, "key-a", &window(), Some(0)),
            Err(TransportError::Verify)
        );
    }

    #[test]
    fn a_malformed_signature_is_refused_before_any_download() {
        for bad in [
            "",
            "not base64!",
            "dGV4dA==",
            &std_b64(b"untrusted comment: x\n"),
        ] {
            let server = Server::new(&[Some(bad)]);
            assert_eq!(
                run(&server, "key-a", &window(), Some(0)),
                Err(TransportError::Verify),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn a_transport_with_one_configured_key_and_no_list_behaves_as_it_always_did() {
        // No key list: the plugin's check against the configured key is all there is, whatever the
        // signature looks like (the plugin refuses it at download if it is wrong).
        let server = Server::new(&[Some("anything at all")]);
        let release = run(&server, "key-a", &[], None).unwrap().unwrap();
        assert_eq!(release.1, "key-a");
        assert_eq!(server.asked_with.borrow().len(), 1);
    }

    #[test]
    fn a_failing_request_is_that_failure_and_never_a_downgrade_to_another_key() {
        let fails = |_: &str| -> Result<Option<(String, String)>, TransportError> {
            Err(TransportError::Network)
        };
        assert_eq!(
            check_across_keys("key-a", &window(), Some(0), fails, |r| r.0.as_str()),
            Err(TransportError::Network)
        );
        let calls = RefCell::new(0);
        let second_fails = |public_key: &str| -> Result<Option<(String, String)>, TransportError> {
            *calls.borrow_mut() += 1;
            if *calls.borrow() == 1 {
                Ok(Some((signature(KEY_B), public_key.to_owned())))
            } else {
                Err(TransportError::Network)
            }
        };
        assert_eq!(
            check_across_keys("key-a", &window(), Some(0), second_fails, |r| r.0.as_str()),
            Err(TransportError::Network)
        );
    }
}
