use super::*;

#[test]
fn search_operators_filter() {
    let conn = test_conn();
    upsert_account(&conn, "a@x.com").unwrap();
    let mut m1 = msg("t1", &["INBOX"], false, 20);
    m1.from_addr = "alice@x.com".into();
    m1.from_name = "Alice".into();
    m1.subject = "Invoice March".into();
    m1.has_attachments = true;
    let mut m2 = msg("t2", &["INBOX", "UNREAD"], true, 10);
    m2.from_addr = "bob@x.com".into();
    m2.from_name = "Bob".into();
    m2.subject = "Lunch".into();
    apply_messages(&conn, "a@x.com", &[m1, m2]).unwrap();

    let only = |q: &str| -> Vec<String> {
        search_threads_scoped(&conn, q, None, None, None)
            .unwrap()
            .into_iter()
            .map(|t| t.thread_id)
            .collect()
    };
    assert_eq!(only("from:alice"), vec!["t1"], "from: matches sender");
    assert_eq!(only("has:attachment"), vec!["t1"], "has:attachment");
    assert_eq!(only("is:unread"), vec!["t2"], "is:unread = thread unread");
    assert_eq!(only("subject:invoice"), vec!["t1"], "subject: substring");
    assert_eq!(only("march from:alice"), vec!["t1"], "free text + operator");
    assert!(
        only("from:alice is:unread").is_empty(),
        "alice's mail is read, so the combined filter excludes it"
    );
}

#[test]
fn search_operators_v1_parity() {
    let conn = test_conn();
    upsert_account(&conn, "a@x.com").unwrap();
    let mut m1 = msg("t1", &["INBOX"], false, 20);
    m1.from_addr = "alice@x.com".into();
    m1.from_name = "Alice".into();
    m1.subject = "Invoice March".into();
    m1.has_attachments = true;
    let mut m2 = msg("t2", &["INBOX", "UNREAD"], true, 10);
    m2.from_addr = "bob@x.com".into();
    m2.from_name = "Bob".into();
    m2.subject = "Lunch tomorrow".into();
    let mut m3 = msg("t3", &["INBOX"], false, 5);
    m3.from_addr = "carol@x.com".into();
    m3.from_name = "Carol".into();
    m3.subject = "Quarterly review".into();
    m3.cc_addrs = "[\"carol@x.com\",\"dave@x.com\"]".into();
    apply_messages(&conn, "a@x.com", &[m1, m2, m3]).unwrap();
    // bcc: has no FetchedMessage field (Gmail's metadata fetch never
    // surfaces Bcc for received/synced mail — see db.rs::upsert_messages).
    // `bcc:` search now reads the normalized message_recipients table
    // (L10), so seed it directly the way a future send-path writer would,
    // rather than the old raw `UPDATE messages SET bcc_addrs=...` (which
    // only ever fed the now-unused header-string LIKE path).
    conn.execute(
        "INSERT INTO message_recipients(account_id, message_id, kind, addr_norm, name_norm)
         VALUES('a@x.com', 't3-5', 'bcc', 'erin@x.com', '')",
        [],
    )
    .unwrap();

    let ids = |q: &str| -> Vec<String> {
        search_threads_scoped(&conn, q, None, None, None)
            .unwrap()
            .into_iter()
            .map(|t| t.thread_id)
            .collect()
    };
    assert_eq!(ids("cc:dave"), vec!["t3"], "cc: reads messages.cc_addrs");
    assert_eq!(
        ids("cc:nobody"),
        Vec::<String>::new(),
        "cc: returns empty when no match"
    );
    assert_eq!(
        ids("in:inbox -cc:dave"),
        vec!["t1", "t2"],
        "-cc: excludes matching threads"
    );
    assert_eq!(
        ids("in:inbox -bcc:erin"),
        vec!["t1", "t2"],
        "-bcc: excludes matching threads"
    );
    assert_eq!(
        ids("filename:invoice"),
        Vec::<String>::new(),
        "filename: requires attachments table rows; test fixtures don't seed them"
    );
    assert_eq!(ids("is:read"), vec!["t1", "t3"], "is:read = !unread");
    assert_eq!(
        ids("after:1970-01-01"),
        vec!["t1", "t2", "t3"],
        "after: inclusive"
    );
    assert_eq!(
        ids("before:2999-01-01"),
        vec!["t1", "t2", "t3"],
        "before: inclusive upper bound"
    );
    assert_eq!(ids("in:inbox"), vec!["t1", "t2", "t3"], "in:inbox resolves");
    // Thread-level negation: the test_conn() helper does not seed fts_messages,
    // so a FTS-anchored search returns nothing for `subj`. Pin the positive
    // anchor on the `subject:` operator (it reads messages.subject directly).
    // t1=Invoice March, t2=Lunch tomorrow, t3=Quarterly review. -from:alice
    // should drop t1 (Alice) but keep t2 and t3 (neither is from Alice).
    // The single-message vs multi-message case is covered by
    // `search_thread_level_negation_multi_message` below.
    assert_eq!(
        ids("subject:tomorrow"),
        vec!["t2"],
        "subject:tomorrow pins t2 only"
    );
    let neg_alice = ids("subject:tomorrow -from:alice");
    assert!(!neg_alice.contains(&"t1".to_string()));
    assert_eq!(
        neg_alice,
        vec!["t2"],
        "t2 matches subject:tomorrow and is not from Alice"
    );
    let neg_alice_invoice = ids("subject:invoice -from:alice");
    assert!(
        !neg_alice_invoice.contains(&"t1".to_string()),
        "t1 (Invoice March, from Alice) is dropped"
    );
    assert!(
        neg_alice_invoice.is_empty(),
        "no other thread has 'invoice' in subject"
    );
    assert_eq!(
        ids("march OR lunch"),
        vec!["t1", "t2"],
        "OR returns either side, ordered by newest thread"
    );
    assert_eq!(
        ids("{ march lunch }"),
        vec!["t1", "t2"],
        "brace groups are OR groups"
    );
}

#[test]
fn search_thread_level_negation_multi_message() {
    let conn = test_conn();
    upsert_account(&conn, "a@x.com").unwrap();

    // Single-message thread from Alice, subject "Quarterly Q1 needle".
    let mut single = msg("single", &["INBOX"], false, 10);
    single.from_addr = "alice@x.com".into();
    single.from_name = "Alice".into();
    single.subject = "Quarterly Q1 needle".into();

    // Multi-message thread: m1 from Bob (no Alice), m2 from Alice.
    let mut m1 = msg("multi", &["INBOX"], false, 20);
    m1.id = "multi-m1".into();
    m1.from_addr = "bob@x.com".into();
    m1.from_name = "Bob".into();
    m1.subject = "Q1 needle review".into();
    m1.internal_date = 20;
    let mut m2 = msg("multi", &["INBOX"], false, 25);
    m2.id = "multi-m2".into();
    m2.from_addr = "alice@x.com".into();
    m2.from_name = "Alice".into();
    m2.subject = "Q1 needle reply".into();
    m2.internal_date = 25;

    // Multi-message thread with NO Alice messages — should survive negation.
    let mut m3 = msg("clean", &["INBOX"], false, 30);
    m3.id = "clean-m1".into();
    m3.from_addr = "carol@x.com".into();
    m3.from_name = "Carol".into();
    m3.subject = "Q1 needle check".into();
    m3.internal_date = 30;
    let mut m4 = msg("clean", &["INBOX"], false, 35);
    m4.id = "clean-m2".into();
    m4.from_addr = "dave@x.com".into();
    m4.from_name = "Dave".into();
    m4.subject = "Q1 needle ack".into();
    m4.internal_date = 35;

    apply_messages(&conn, "a@x.com", &[single, m1, m2, m3, m4]).unwrap();

    let ids = |q: &str| -> Vec<String> {
        search_threads_scoped(&conn, q, None, None, None)
            .unwrap()
            .into_iter()
            .map(|t| t.thread_id)
            .collect()
    };

    // All three threads match the FTS term "needle".
    let baseline = ids("needle");
    assert!(baseline.contains(&"single".to_string()));
    assert!(baseline.contains(&"multi".to_string()));
    assert!(baseline.contains(&"clean".to_string()));

    // -from:alice drops BOTH the single-Alice thread AND the multi-message
    // thread that contains an Alice reply. The "clean" thread (no Alice) stays.
    let after = ids("needle -from:alice");
    assert!(
        !after.contains(&"single".to_string()),
        "single-msg Alice thread dropped"
    );
    assert!(
        !after.contains(&"multi".to_string()),
        "multi-msg thread with Alice reply dropped"
    );
    assert!(
        after.contains(&"clean".to_string()),
        "clean thread survives"
    );
}

#[test]
fn search_thread_level_negation_text_term() {
    let conn = test_conn();
    upsert_account(&conn, "a@x.com").unwrap();

    // Multi-msg thread where subject/snippet don't contain the word but body
    // does. `negated_text` is the bug the old per-message NOT LIKE path got
    // wrong: it excluded the thread only if EVERY message failed, so a single
    // message without the term was enough to keep the thread.
    let mut m1 = msg("tx", &["INBOX"], false, 10);
    m1.id = "tx-m1".into();
    m1.subject = "Sync issue".into();
    m1.snippet = "fwd of upstream notice".into();
    let mut m2 = msg("tx", &["INBOX"], false, 20);
    m2.id = "tx-m2".into();
    m2.subject = "Re: Sync issue".into();
    m2.snippet = "now resolved on our side".into();
    let mut m3 = msg("ty", &["INBOX"], false, 30);
    m3.id = "ty-m1".into();
    m3.subject = "Sync issue elsewhere".into();
    m3.snippet = "different incident, no impact".into();
    apply_messages(&conn, "a@x.com", &[m1, m2, m3]).unwrap();

    let ids = |q: &str| -> Vec<String> {
        search_threads_scoped(&conn, q, None, None, None)
            .unwrap()
            .into_iter()
            .map(|t| t.thread_id)
            .collect()
    };

    // Positive anchor: all three threads mention "sync" in subject/snippet.
    let baseline = ids("sync");
    assert!(baseline.contains(&"tx".to_string()));
    assert!(baseline.contains(&"ty".to_string()));

    // `-resolved` should drop the tx thread (m2 contains "resolved") but
    // keep ty (which has no "resolved" anywhere). The old per-message NOT
    // LIKE would have kept tx because m1 does not contain "resolved".
    let after = ids("sync -resolved");
    assert!(
        !after.contains(&"tx".to_string()),
        "tx contains 'resolved' in m2; thread dropped"
    );
    assert!(
        after.contains(&"ty".to_string()),
        "ty has no 'resolved'; thread survives"
    );
}

#[test]
fn search_label_positive_and_negated() {
    let conn = test_conn();
    upsert_account(&conn, "a@x.com").unwrap();
    // Three threads: one with the custom label "Receipts", one without, one
    // in TRASH (label removed on archive out). t1/t2 use subject:tomorrow to
    // anchor, t3 uses subject:review — keeps the FTS-anchored test path off
    // fts_messages which test_conn() doesn't seed.
    let mut m1 = msg("lpos", &["INBOX", "Receipts"], false, 10);
    m1.subject = "tomorrow's receipts".into();
    let mut m2 = msg("lneg", &["INBOX"], false, 20);
    m2.subject = "tomorrow's notes".into();
    let mut m3 = msg("ldrop", &["INBOX", "Receipts"], false, 30);
    m3.subject = "review the receipts".into();
    apply_messages(&conn, "a@x.com", &[m1, m2, m3]).unwrap();

    let ids = |q: &str| -> Vec<String> {
        search_threads_scoped(&conn, q, None, None, None)
            .unwrap()
            .into_iter()
            .map(|t| t.thread_id)
            .collect()
    };

    // Positive `label:Receipts` returns threads whose thread_labels row carries
    // the label. t1 and t3 are labeled; t2 is not. Order is last_message_at
    // DESC, so ldrop (date=30) precedes lpos (date=10).
    let r = ids("subject:receipts");
    assert_eq!(r.len(), 2);
    assert!(r.contains(&"lpos".to_string()));
    assert!(r.contains(&"ldrop".to_string()));
    let lpos = ids("subject:receipts label:Receipts");
    assert_eq!(lpos.len(), 2, "positive label restricts to labeled threads");
    assert!(lpos.contains(&"lpos".to_string()));
    assert!(lpos.contains(&"ldrop".to_string()));
    assert_eq!(
        ids("subject:receipts -label:Receipts"),
        Vec::<String>::new(),
        "negated label excludes every labeled thread in scope"
    );
    // Anchor on t1's own subject and assert -label:Receipts drops it.
    let subj_tomorrow = ids("subject:tomorrow");
    assert_eq!(subj_tomorrow.len(), 2);
    assert!(subj_tomorrow.contains(&"lpos".to_string()));
    assert!(subj_tomorrow.contains(&"lneg".to_string()));
    let neg = ids("subject:tomorrow -label:Receipts");
    assert_eq!(
        neg,
        vec!["lneg"],
        "t1 (labeled Receipts) is dropped; t2 (no Receipts) survives"
    );
}
