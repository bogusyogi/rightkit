// rightkit-sync-keys 0.1.0 consumed by exact registry pin, outside the workspace.
//
// Proves the in-memory key families (rightkit-sync-keys lib.rs): DataKey::from_bytes (version 0
// refused), version, key_id, expose_bytes; DataKey::zone_key; account_scope; StaticKeys with the
// KeyProvider trait (highest version current, key_by_id hit and miss); format_code; and the
// device-approval transfer end to end in process: TransferContext, Requester::begin, receive_offer,
// code, accept; Approver::receive_begin, receive_reveal, code, approve. Also a wrong typed code
// refused with SasMismatch, and a spent session refused.
// Does not prove: KeyManager (create_if_absent, rotate, install, retire, forget_all) and ZoneKeys,
// which need a rightkit-credentials Store and so the OS credential store; the relay's session
// expiry and rate limiting; the recovery-phrase wrap (not in this crate); the other TransferContext
// and protocol error paths; any network.
// State: in-process only. Key bytes are fixed constants. The transfer draws its nonces and ephemeral
// keys from the OS random source, so the six-digit code differs per run, but every assertion holds
// for any code. No keychain, no files, no network, no real user data.
use rightkit_sync_keys::{
    account_scope, format_code, Approver, DataKey, Error, KeyProvider, Requester, StaticKeys,
    TransferContext, KEY_LEN,
};
use zeroize::Zeroizing;

fn key(version: u32, fill: u8) -> Result<DataKey, Error> {
    DataKey::from_bytes(version, Zeroizing::new([fill; KEY_LEN]))
}

// One transfer session after the reveal: both screens now show a code.
fn paired(ctx: &TransferContext) -> Result<(Requester, Approver), Error> {
    let (mut requester, begin) = Requester::begin(ctx.clone(), Some("demo requester"))?;
    let (mut approver, offer) = Approver::receive_begin(ctx.clone(), &begin)?;
    let reveal = requester.receive_offer(&offer)?;
    approver.receive_reveal(&reveal)?;
    Ok((requester, approver))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Key material: the key id is a function of the bytes and the version is kept.
    let first = key(1, 7)?;
    assert_eq!(first.version(), 1);
    assert_eq!(first.key_id(), key(1, 7)?.key_id(), "same bytes, same key id");
    assert_ne!(first.key_id(), key(1, 8)?.key_id(), "different bytes, different key id");
    assert!(
        matches!(key(0, 7), Err(Error::Invalid { .. })),
        "versions start at 1"
    );

    // Zone keys: deterministic per (app, zone), unrelated across zones, tagged with the data key.
    let notes = first.zone_key("demo-app", "notes")?;
    let notes_again = first.zone_key("demo-app", "notes")?;
    let calendar = first.zone_key("demo-app", "calendar")?;
    assert_eq!(notes.as_bytes(), notes_again.as_bytes(), "zone keys are deterministic");
    assert_ne!(notes.as_bytes(), calendar.as_bytes(), "zones are unrelated keys");
    assert_eq!(notes.key_id(), first.key_id());
    assert_eq!(notes.version(), 1);

    // Engine scope: deterministic, 64 hex characters, changed by a rotation to a new key id.
    let scope = account_scope("demo-app", "notes", "demo-account", first.key_id())?;
    assert_eq!(scope.len(), 64, "SHA-256 as lowercase hex");
    assert_eq!(
        scope,
        account_scope("demo-app", "notes", "demo-account", first.key_id())?
    );
    let rotated = key(2, 9)?;
    assert_ne!(
        scope,
        account_scope("demo-app", "notes", "demo-account", rotated.key_id())?,
        "a rotated key changes the sync scope"
    );

    // KeyProvider over fixed keys: the highest version is current, an older id still resolves.
    let provider = StaticKeys::new(vec![first.clone(), rotated.clone()], "demo-app", "notes")?;
    let current = provider.current_key()?;
    assert_eq!(current.version(), 2, "the highest version is current");
    assert_eq!(current.key_id(), rotated.key_id());
    let older = provider
        .key_by_id(first.key_id())?
        .ok_or("version 1 is held")?;
    assert_eq!(older.as_bytes(), notes.as_bytes(), "an older key resolves by id");
    let unknown = key(3, 5)?;
    assert!(
        provider.key_by_id(unknown.key_id())?.is_none(),
        "an id not held is None"
    );

    // Display grouping of the six-digit code.
    assert_eq!(format_code("123456"), "123 456");

    // Transfer, wrong code first: the approver refuses a typed code that is not the requester's.
    let ctx = TransferContext::new("demo-app", "demo-account")?;
    let (mut requester, mut approver) = paired(&ctx)?;
    let requester_code = requester.code()?;
    let approver_code = approver.code()?;
    assert_eq!(requester_code, approver_code, "both screens show the same code");
    let wrong = if requester_code == "000001" { "000002" } else { "000001" };
    assert_eq!(
        approver.approve(wrong, &first).unwrap_err(),
        Error::SasMismatch { remaining: 2 },
        "a wrong code wraps nothing and leaves two tries"
    );

    // Transfer, right code: the requester opens the wrapped key and gets the same key back.
    let transfer = approver.approve(&requester_code, &first)?;
    let received = requester.accept(&approver_code, &transfer)?;
    assert_eq!(received.version(), first.version());
    assert_eq!(received.key_id(), first.key_id());
    assert_eq!(received.expose_bytes(), first.expose_bytes());

    // One transfer per session: the approver's session is spent.
    assert!(
        matches!(
            approver.approve(&requester_code, &first),
            Err(Error::Protocol(_))
        ),
        "a spent session approves nothing"
    );

    println!("OK rightkit-sync-keys 0.1.0 (registry consumer)");
    Ok(())
}
