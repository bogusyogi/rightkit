// rightkit-approvals 0.2.0 consumed by exact registry pin (default features), outside the workspace.
use std::cell::{Cell, RefCell};

use rightkit_approvals::rules::{self, Decision, Limits, Outcome};
use rightkit_approvals::{
    bind_of, new_id, Audit, Audited, Binding, Change, Error, FileStore, Grant, MemoryStore, Origin,
    Proof, Record, Refusal, Request, Retention, State, Store,
};

fn check(ok: bool, what: &str) {
    if ok {
        println!("ok: {what}");
    } else {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
}

const DOMAIN: &str = "consumer.approval.v1";

fn request(id: &str, client: &str, args: &str, now: u64) -> Request {
    Request {
        id: id.to_string(),
        kind: "act".into(),
        scope: "files.write".into(),
        origin: Origin {
            door: "local".into(),
            filer: "cli".into(),
            client: client.into(),
        },
        binding: Binding::new(args, "notes.txt"),
        summary: "write notes.txt".into(),
        created_ms: now,
        expires_ms: now + 60_000,
        ..Request::default()
    }
}

fn approve<'a>(shown: Option<&'a str>, proof: Option<&'a dyn Proof>) -> Decision<'a> {
    Decision {
        approve: true,
        shown,
        domain: DOMAIN,
        proof,
        grant: Grant::Once { valid_ms: 60_000 },
        reason: None,
    }
}

struct Accept(bool);

impl Proof for Accept {
    fn verify(&self, _record: &Record) -> rightkit_approvals::Result<()> {
        if self.0 {
            Ok(())
        } else {
            Err(Refusal::new("proof_refused", "not the decider"))
        }
    }
}

/// An audit log that can be made to fail.
#[derive(Default)]
struct Log {
    rows: RefCell<Vec<String>>,
    down: Cell<bool>,
}

impl Audit for Log {
    fn append(&self, record: &Record, change: Option<&Change>) -> rightkit_approvals::Result<()> {
        if self.down.get() {
            return Err(Refusal::new("audit_down", "the audit log is unavailable"));
        }
        let to = change.map_or("filed", |c| c.to.as_str());
        self.rows
            .borrow_mut()
            .push(format!("{} {to}", record.request.id));
        Ok(())
    }
}

const KEEP: Retention = Retention {
    max_pending: 8,
    max_decided: 8,
    max_done: 8,
    closed_visible_ms: 60_000,
};

fn main() {
    let ab = bind_of(&[b"ab", b"c"]);
    check(
        ab != bind_of(&[b"a", b"bc"]),
        "bind_of length-prefixes its parts",
    );
    check(
        ab.len() == 64
            && ab
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
        "bind_of is 64 lowercase hex",
    );
    let (one, two) = (new_id().unwrap(), new_id().unwrap());
    check(
        one.len() == 32 && one != two,
        "new_id gives distinct 32-hex ids",
    );

    let bound = Binding {
        expires_ms: 1_000,
        revision: 2,
        ..Binding::new("aa", "dest")
    };
    let same = Binding {
        revision: 2,
        ..Binding::new("aa", "dest")
    };
    check(bound.check(&same, 999).is_ok(), "the same binding passes");
    check(
        bound.check(
            &Binding {
                destination: "x".into(),
                ..same.clone()
            },
            0,
        ) == Err(Error::Different),
        "another destination is Different",
    );
    check(
        bound.check(
            &Binding {
                args: "bb".into(),
                ..same.clone()
            },
            0,
        ) == Err(Error::Changed),
        "other arguments are Changed",
    );
    check(
        bound.check(
            &Binding {
                revision: 3,
                ..same.clone()
            },
            0,
        ) == Err(Error::Changed),
        "another revision is Changed",
    );
    check(
        bound.check(&same, 1_001) == Err(Error::Expired),
        "a past expiry is Expired",
    );

    let store = MemoryStore::new(KEEP);
    let limits = Limits {
        pending_cap: 2,
        per_client: false,
    };
    let now = rightkit_approvals::now_ms();
    let first = rules::file(&store, request("r1", "chat:1", "aa", now), &limits, now).unwrap();
    let again = rules::file(&store, request("r2", "chat:1", "aa", now), &limits, now).unwrap();
    check(
        first.is_new() && !again.is_new() && again.id() == "r1",
        "a repeat gets the same id",
    );
    rules::file(&store, request("r3", "chat:2", "bb", now), &limits, now).unwrap();
    check(
        rules::file(&store, request("r4", "chat:3", "cc", now), &limits, now) == Err(Error::Busy),
        "the pending cap refuses",
    );

    let r1 = store.get("r1").unwrap().unwrap();
    let digest = r1.request.display_digest(DOMAIN);
    check(
        rules::decide(&store, "r1", &approve(Some("stale"), None), now) == Err(Error::Shown),
        "a decision on what was not shown is refused",
    );
    check(
        store.get("r1").unwrap().unwrap().state == State::Pending,
        "and the request stays pending",
    );
    check(
        rules::decide(
            &store,
            "r1",
            &approve(Some(&digest), Some(&Accept(false))),
            now,
        )
        .is_err_and(|e| e.refusal().code == "proof_refused"),
        "a proof that refuses blocks the decision",
    );
    let decided = rules::decide(&store, "r1", &approve(Some(&digest), None), now).unwrap();
    check(
        decided.is_some_and(|r| r.state == State::Approved),
        "the decision on what was shown approves",
    );

    let mut other = request("", "chat:1", "aa", now);
    other.origin.door = "gateway".into();
    check(
        rules::take(&store, "r1", &other, None, false, now) == Err(Error::Different),
        "another door is Different",
    );
    check(
        rules::take(
            &store,
            "r1",
            &request("", "chat:1", "zz", now),
            None,
            false,
            now,
        ) == Err(Error::Changed),
        "other arguments are Changed",
    );
    let used = rules::take(
        &store,
        "r1",
        &request("", "chat:1", "aa", now),
        None,
        false,
        now,
    );
    check(
        used.is_ok_and(|r| r.state == State::Done),
        "the approval is used once",
    );
    check(
        rules::take(
            &store,
            "r1",
            &request("", "chat:1", "aa", now),
            None,
            false,
            now,
        ) == Err(Error::Used),
        "a second use is Used",
    );
    check(
        rules::take(
            &store,
            "r3",
            &request("", "chat:2", "bb", now),
            None,
            false,
            now,
        ) == Err(Error::Pending),
        "a pending request without a proof is Pending",
    );
    let one_move = rules::take(
        &store,
        "r3",
        &request("", "chat:2", "bb", now),
        Some(&Accept(true)),
        false,
        now,
    );
    check(
        one_move.is_ok_and(|r| r.state == State::Done),
        "with a proof a pending request is decided and used in one move",
    );

    let mut session = request("s1", "chat:9", "", now);
    session.kind = "session".into();
    rules::file(&store, session, &limits, now).unwrap();
    let grant = Decision {
        grant: Grant::Idle { idle_ms: 60_000 },
        ..approve(None, None)
    };
    rules::decide(&store, "s1", &grant, now).unwrap();
    let mine = Origin {
        door: "local".into(),
        filer: String::new(),
        client: "chat:9".into(),
    };
    let theirs = Origin {
        client: "chat:8".into(),
        ..mine.clone()
    };
    check(
        rules::session(&store, "session", "files.write", &mine, now)
            .unwrap()
            .is_some(),
        "a session grant is found for its client",
    );
    check(
        rules::session(&store, "session", "files.write", &theirs, now)
            .unwrap()
            .is_none(),
        "and not for another client",
    );

    // Claim, settle, recover.
    let roomy = Limits {
        pending_cap: 8,
        per_client: false,
    };
    for id in ["c1", "c2", "c3", "v1"] {
        rules::file(&store, request(id, "chat:4", id, now), &roomy, now).unwrap();
        rules::decide(&store, id, &approve(None, None), now).unwrap();
    }
    let act = |args: &str| request("", "chat:4", args, now);
    check(
        rules::claim(&store, "c1", &act("c1"), None, false, now)
            .is_ok_and(|r| r.state == State::Claimed),
        "an approval is claimed by one act",
    );
    check(
        rules::claim(&store, "c1", &act("c1"), None, false, now) == Err(Error::Used),
        "a second claim is Used",
    );
    check(
        rules::take(&store, "c1", &act("c1"), None, false, now) == Err(Error::Used),
        "a claimed approval cannot be taken",
    );
    check(
        rules::revoke(&store, "c1", "revoked", now)
            .unwrap()
            .is_none(),
        "a claimed approval is past revoking",
    );
    check(
        rules::settle(&store, "c1", Outcome::Done, Some("msg-1"), now)
            .is_ok_and(|r| r.state == State::Done && r.result.as_deref() == Some("msg-1")),
        "a claim is settled done with its result",
    );
    check(
        rules::settle(&store, "c1", Outcome::Failed, None, now) == Err(Error::Unclaimed),
        "a second settlement is Unclaimed",
    );
    rules::claim(&store, "c2", &act("c2"), None, false, now).unwrap();
    rules::claim(&store, "c3", &act("c3"), None, false, now).unwrap();
    let recovered = rules::recover(&store, now, now + 1, |r| {
        if r.request.id == "c3" {
            Outcome::Failed
        } else {
            Outcome::Unknown
        }
    })
    .unwrap();
    check(
        recovered.len() == 2 && recovered.iter().all(|r| r.state == State::Failed),
        "recover settles every claim nobody settled",
    );
    let reason = |id: &str| store.get(id).unwrap().unwrap().reason;
    check(
        reason("c2").as_deref() == Some("unknown_outcome")
            && reason("c3").as_deref() == Some("failed"),
        "as the reconciler answers, unknown when it cannot tell",
    );
    check(
        rules::settle(&store, "c2", Outcome::Done, None, now) == Err(Error::Unclaimed),
        "a settlement after recovery is Unclaimed",
    );

    // Revoke.
    check(
        rules::revoke(&store, "v1", "revoked", now)
            .unwrap()
            .is_some_and(|r| r.state == State::Denied && r.reason.as_deref() == Some("revoked")),
        "an approved request is revoked before use",
    );
    check(
        rules::take(&store, "v1", &act("v1"), None, false, now) == Err(Error::Denied),
        "a revoked approval is Denied at use",
    );
    rules::file(&store, request("w1", "chat:5", "w1", now), &roomy, now).unwrap();
    rules::file(&store, request("w2", "chat:5", "w2", now), &roomy, now).unwrap();
    rules::decide(&store, "w2", &approve(None, None), now).unwrap();
    check(
        rules::revoke_where(
            &store,
            |r| r.request.origin.client == "chat:5",
            "cancelled",
            now,
        )
        .unwrap()
        .len()
            == 2,
        "revoke_where withdraws every pending or approved request it selects",
    );

    // Audit before effect.
    let audited = Audited::new(MemoryStore::new(KEEP), Log::default());
    rules::file(&audited, request("a1", "chat:6", "a1", now), &roomy, now).unwrap();
    rules::decide(&audited, "a1", &approve(None, None), now).unwrap();
    audited.audit().down.set(true);
    let a1 = request("", "chat:6", "a1", now);
    check(
        rules::claim(&audited, "a1", &a1, None, false, now)
            .is_err_and(|e| e.refusal().code == "audit_down"),
        "a failed audit append stops the move",
    );
    check(
        audited.get("a1").unwrap().unwrap().state == State::Approved,
        "and the approval is unchanged",
    );
    audited.audit().down.set(false);
    rules::claim(&audited, "a1", &a1, None, false, now).unwrap();
    check(
        *audited.audit().rows.borrow() == ["a1 filed", "a1 approved", "a1 claimed"],
        "every filing and move has its row first",
    );

    let root = std::env::temp_dir().join(format!(
        "rightkit-approvals-consumer-{}",
        std::process::id()
    ));
    let files = FileStore::new(&root);
    let id = new_id().unwrap();
    let mut on_disk = request(&id, "", "cafe", now);
    on_disk.binding.destination = "draft-1".into();
    files.insert(&Record::pending(on_disk.clone())).unwrap();
    let path = files.path_of("act", "draft-1", &id).unwrap();
    let body: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    check(
        body["nonce"] == id.as_str() && body["input_hash"] == "cafe",
        "the request file keeps nonce and input_hash",
    );
    let take =
        |store: &FileStore| rules::take(store, &id, &on_disk, Some(&Accept(true)), false, now);
    check(take(&files).is_ok(), "a file-store request is used");
    let mut used_marker = path.clone().into_os_string();
    used_marker.push(".used");
    check(
        std::path::Path::new(&used_marker).is_file(),
        "the use leaves a .used marker",
    );
    check(take(&files).is_err(), "a second use loses");
    let id2 = new_id().unwrap();
    let mut claimed_on_disk = request(&id2, "", "beef", now);
    claimed_on_disk.binding.destination = "draft-2".into();
    files
        .insert(&Record::pending(claimed_on_disk.clone()))
        .unwrap();
    rules::claim(
        &files,
        &id2,
        &claimed_on_disk,
        Some(&Accept(true)),
        false,
        now,
    )
    .unwrap();
    let mut claimed_marker = files
        .path_of("act", "draft-2", &id2)
        .unwrap()
        .into_os_string();
    claimed_marker.push(".claimed");
    check(
        std::path::Path::new(&claimed_marker).is_file() && files.claimed().unwrap().len() == 1,
        "a file-store claim leaves a .claimed marker",
    );
    check(
        rules::settle(&files, &id2, Outcome::Unknown, None, now)
            .is_ok_and(|r| r.state == State::Failed),
        "and is settled once",
    );
    check(
        files.get(&id2).unwrap().unwrap().reason.as_deref() == Some("unknown_outcome")
            && files.claimed().unwrap().is_empty(),
        "an unknown outcome is read back from the file",
    );
    let _ = std::fs::remove_dir_all(&root);
    println!("rightkit-approvals consumer: all checks passed");
}
