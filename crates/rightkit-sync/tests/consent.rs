//! Consent gate on an account/scope change: nothing is published into a new account until the
//! app records consent. Fake provider: two in-memory zones behind one switchable account.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use rightkit_sync::*;

#[derive(Clone, Debug, PartialEq)]
struct Note {
    id: String,
    text: String,
    created: i64,
    updated: i64,
}

struct NoteMapper;
impl RecordMapper for NoteMapper {
    type Item = Note;
    fn record_type(&self) -> &str {
        "Note"
    }
    fn header(&self, n: &Note) -> ItemHeader {
        ItemHeader {
            id: n.id.clone(),
            created_at_ms: n.created,
            updated_at_ms: n.updated,
            origin_device: None,
        }
    }
    fn encode(&self, n: &Note, r: &mut RawRecord) {
        r.set("text", FieldValue::Text(n.text.clone()));
    }
    fn decode(&self, r: &RawRecord) -> Result<Note, String> {
        Ok(Note {
            id: r.text("id").ok_or("no id")?,
            text: r.text("text").ok_or("no text")?,
            created: r.date_ms("createdAt").ok_or("no createdAt")?,
            updated: r.date_ms("updatedAt").ok_or("no updatedAt")?,
        })
    }
}

#[derive(Default, Clone)]
struct Local(Arc<Mutex<BTreeMap<String, Note>>>);
impl Local {
    fn put(&self, id: &str, text: &str, at: i64) {
        self.0.lock().unwrap().insert(
            id.into(),
            Note {
                id: id.into(),
                text: text.into(),
                created: at,
                updated: at,
            },
        );
    }
    fn has(&self, id: &str) -> bool {
        self.0.lock().unwrap().contains_key(id)
    }
}
impl LocalStore<Note> for Local {
    fn all(&self) -> SyncResult<Vec<Note>> {
        Ok(self.0.lock().unwrap().values().cloned().collect())
    }
    fn remove(&self, id: &str) -> SyncResult<()> {
        self.0.lock().unwrap().remove(id);
        Ok(())
    }
    fn apply_remote(&self, items: &[Note]) -> SyncResult<Vec<ApplyOutcome>> {
        let mut m = self.0.lock().unwrap();
        for n in items {
            m.insert(n.id.clone(), n.clone());
        }
        Ok(items.iter().map(|_| ApplyOutcome::Applied).collect())
    }
}

/// Which account is signed in: index into the zones, plus the auth generation.
#[derive(Clone, Default)]
struct Account {
    index: Arc<AtomicUsize>,
    generation: Arc<AtomicU64>,
}
impl Account {
    fn sign_in(&self, index: usize) {
        self.index.store(index, Ordering::SeqCst);
        self.generation.fetch_add(1, Ordering::SeqCst);
    }
    fn scope(&self) -> String {
        format!("acct-{}", self.index.load(Ordering::SeqCst))
    }
}
impl AuthProvider for Account {
    fn availability(&self) -> SyncResult<()> {
        Ok(())
    }
    fn generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }
    fn account_scope(&self) -> Option<String> {
        Some(self.scope())
    }
}

/// Routes every call to the zone of the signed-in account.
struct Zones {
    zones: Vec<MemoryBackend>,
    account: Account,
}
impl Zones {
    fn cur(&self) -> &MemoryBackend {
        &self.zones[self.account.index.load(Ordering::SeqCst)]
    }
}
impl Backend for Zones {
    fn prepare(&self) -> SyncResult<()> {
        self.cur().prepare()
    }
    fn fetch_changes(&self, since: Option<&[u8]>) -> SyncResult<ChangePage> {
        self.cur().fetch_changes(since)
    }
    fn fetch_record(&self, name: &str) -> SyncResult<Option<RawRecord>> {
        self.cur().fetch_record(name)
    }
    fn save_records(&self, r: &[RawRecord], p: SavePolicy) -> SyncResult<Vec<RecordOutcome>> {
        self.cur().save_records(r, p)
    }
}

#[derive(Default)]
struct Events(Mutex<Vec<String>>);
impl SyncObserver for Events {
    fn event(&self, name: &'static str, _detail: &str) {
        self.0.lock().unwrap().push(name.to_string());
    }
}
impl Events {
    fn count(&self, name: &str) -> usize {
        self.0.lock().unwrap().iter().filter(|n| *n == name).count()
    }
}

struct Switched(Mutex<Vec<(String, String)>>);
impl PassHooks for Switched {
    fn on_account_switch(&self, from: &str, to: &str) {
        self.0.lock().unwrap().push((from.into(), to.into()));
    }
}

struct Rig {
    local: Local,
    storage: MemoryStorage,
    account: Account,
    zones: Vec<MemoryBackend>,
    events: Arc<Events>,
    switched: Arc<Switched>,
    engine: SyncEngine<NoteMapper>,
}

fn engine(
    zones: &[MemoryBackend],
    account: &Account,
    local: &Local,
    storage: &MemoryStorage,
    events: &Arc<Events>,
    switched: &Arc<Switched>,
    gate: bool,
) -> SyncEngine<NoteMapper> {
    SyncEngineBuilder::new(
        Arc::new(NoteMapper),
        Arc::new(Zones {
            zones: zones.to_vec(),
            account: account.clone(),
        }),
        Arc::new(local.clone()),
        Box::new(storage.clone()),
    )
    .auth(Arc::new(account.clone()))
    .observer(events.clone())
    .hooks(switched.clone())
    .config(SyncConfig {
        require_consent_on_scope_change: gate,
        ..SyncConfig::default()
    })
    .build()
    .unwrap()
}

fn rig(gate: bool) -> Rig {
    let zones = vec![MemoryBackend::new(), MemoryBackend::new()];
    let account = Account::default();
    let local = Local::default();
    let storage = MemoryStorage::default();
    let events = Arc::new(Events::default());
    let switched = Arc::new(Switched(Mutex::new(Vec::new())));
    let engine = engine(&zones, &account, &local, &storage, &events, &switched, gate);
    Rig {
        local,
        storage,
        account,
        zones,
        events,
        switched,
        engine,
    }
}

/// Account 0 has synced note `a`; account 1 (with its own note `z`) is then signed in.
fn synced_then_switched(gate: bool) -> Rig {
    let mut r = rig(gate);
    r.zones[1].put_record({
        let mut rec = RawRecord::new("Note", "z");
        rec.set("id", FieldValue::Text("z".into()));
        rec.set("text", FieldValue::Text("theirs".into()));
        rec.set("createdAt", FieldValue::Date(5));
        rec.set("updatedAt", FieldValue::Date(5));
        rec
    });
    r.local.put("a", "mine", 10);
    assert_eq!(r.engine.run_pass().outcome, PassOutcome::Synced);
    assert_eq!(r.zones[0].record_count(), 1);
    r.account.sign_in(1);
    r
}

#[test]
fn scope_change_pauses_everything_until_consent() {
    let mut r = synced_then_switched(true);
    r.local.put("b", "new while switched", 20);
    r.engine.note_changed("b").unwrap();
    let before = (r.zones[1].save_calls(), r.zones[1].record_count());

    let rep = r.engine.run_pass();
    let req = ConsentRequest {
        from: "acct-0".into(),
        to: "acct-1".into(),
    };
    assert_eq!(rep.outcome, PassOutcome::ConsentRequired(req.clone()));
    assert_eq!((rep.uploaded, rep.downloaded), (0, 0));
    assert_eq!(
        (r.zones[1].save_calls(), r.zones[1].record_count()),
        before,
        "nothing may reach the new account"
    );
    assert!(!r.local.has("z"), "nothing downloaded either");
    assert_eq!(r.engine.consent_request(), Some(req));
    assert_eq!(r.engine.ledger().scope.as_deref(), Some("acct-0"));
    assert!(r.switched.0.lock().unwrap().is_empty());
    // Repeat passes stay paused and do not repeat the event.
    assert!(matches!(
        r.engine.run_pass().outcome,
        PassOutcome::ConsentRequired(_)
    ));
    assert_eq!(r.events.count("consent_required"), 1);
    assert_eq!(r.zones[1].save_calls(), before.0);
}

#[test]
fn merge_publishes_local_history_into_the_new_account() {
    let mut r = synced_then_switched(true);
    assert!(matches!(
        r.engine.run_pass().outcome,
        PassOutcome::ConsentRequired(_)
    ));
    r.engine
        .grant_consent("acct-1", ConsentChoice::Merge)
        .unwrap();
    assert_eq!(r.engine.consent_request(), None);
    assert_eq!(
        *r.switched.0.lock().unwrap(),
        vec![("acct-0".to_string(), "acct-1".to_string())]
    );
    let rep = r.engine.run_pass();
    assert_eq!(rep.outcome, PassOutcome::Synced);
    assert!(r.zones[1].record("a").is_some(), "local note was merged in");
    assert!(r.local.has("z"), "new account content downloaded");
    assert_eq!(r.engine.ledger().scope.as_deref(), Some("acct-1"));
    // The old account's zone is untouched.
    assert_eq!(r.zones[0].record_count(), 1);
}

#[test]
fn start_fresh_keeps_existing_local_items_out_of_the_new_account() {
    let mut r = synced_then_switched(true);
    r.engine.run_pass();
    r.engine
        .grant_consent("acct-1", ConsentChoice::StartFresh)
        .unwrap();
    r.local.put("c", "after consent", 30);
    let rep = r.engine.run_pass();
    assert_eq!(rep.outcome, PassOutcome::Synced);
    assert!(r.zones[1].record("a").is_none(), "pre-existing item withheld");
    assert!(r.zones[1].record("c").is_some(), "later items sync normally");
    assert!(r.local.has("a"), "nothing local is deleted");
    assert!(r.local.has("z"));
}

#[test]
fn consent_is_refused_without_a_request_or_for_another_account() {
    let mut r = synced_then_switched(true);
    // Nothing pending before a pass noticed the switch.
    assert!(r
        .engine
        .grant_consent("acct-1", ConsentChoice::Merge)
        .is_err());
    r.engine.run_pass();
    assert!(r
        .engine
        .grant_consent("acct-9", ConsentChoice::Merge)
        .is_err());
    // The account moved on again after the request: that consent is stale.
    r.account.index.store(0, Ordering::SeqCst);
    assert!(r
        .engine
        .grant_consent("acct-1", ConsentChoice::Merge)
        .is_err());
    assert_eq!(r.zones[1].record_count(), 1, "still nothing published");
}

#[test]
fn signing_back_in_to_the_bound_account_clears_the_pause() {
    let mut r = synced_then_switched(true);
    r.engine.run_pass();
    assert!(r.engine.consent_request().is_some());
    r.account.sign_in(0);
    assert_eq!(r.engine.run_pass().outcome, PassOutcome::Synced);
    assert_eq!(r.engine.consent_request(), None);
}

#[test]
fn pending_consent_and_bound_identity_survive_a_restart() {
    let mut r = synced_then_switched(true);
    r.engine.run_pass();
    let restarted = engine(
        &r.zones,
        &r.account,
        &r.local,
        &r.storage,
        &r.events,
        &r.switched,
        true,
    );
    assert_eq!(restarted.ledger().scope.as_deref(), Some("acct-0"));
    assert_eq!(
        restarted.consent_request().map(|c| c.to),
        Some("acct-1".to_string())
    );
    let mut restarted = restarted;
    assert!(matches!(
        restarted.run_pass().outcome,
        PassOutcome::ConsentRequired(_)
    ));
    assert_eq!(r.zones[1].record_count(), 1);
}

#[test]
fn gate_is_off_by_default_and_the_old_silent_reset_remains() {
    let mut r = synced_then_switched(false);
    let rep = r.engine.run_pass();
    assert_eq!(rep.outcome, PassOutcome::Synced);
    assert!(r.zones[1].record("a").is_some());
    assert_eq!(r.events.count("consent_required"), 0);
}

#[test]
fn first_bind_needs_no_consent() {
    let mut r = rig(true);
    r.local.put("a", "mine", 10);
    assert_eq!(r.engine.run_pass().outcome, PassOutcome::Synced);
    assert_eq!(r.engine.ledger().scope.as_deref(), Some("acct-0"));
}
