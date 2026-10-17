#![cfg(feature = "sql")]

use rightkit_app_settings::{Error, Schema, SqlError, SqlSettings};
use rightkit_db::rusqlite::Connection;
use serde_json::Value;

fn conn() -> Connection {
    Connection::open_in_memory().unwrap()
}

fn plain() -> SqlSettings {
    SqlSettings::new(Schema::new("mail", 1))
}

#[test]
fn set_get_remove_and_dynamic_keys() {
    let c = conn();
    let s = plain();
    s.ensure_table(&c).unwrap();
    assert_eq!(s.get(&c, "theme").unwrap(), None);
    s.set(&c, "theme", "dark").unwrap();
    s.set(&c, "signature:acct1", "A").unwrap();
    s.set(&c, "signature:acct2", "B").unwrap();
    s.set(&c, "theme", "light").unwrap();
    assert_eq!(s.get(&c, "theme").unwrap().as_deref(), Some("light"));
    let sigs = s.list(&c, "signature:").unwrap();
    assert_eq!(
        sigs,
        vec![
            ("signature:acct1".to_string(), "A".to_string()),
            ("signature:acct2".to_string(), "B".to_string())
        ]
    );
    assert!(s.remove(&c, "theme").unwrap());
    assert!(!s.remove(&c, "theme").unwrap());
}

#[test]
fn missing_table_reads_defaults_and_write_errors() {
    let c = conn();
    let s = plain().default_value("theme", "dark");
    assert_eq!(s.get(&c, "theme").unwrap().as_deref(), Some("dark"));
    assert!(s.list(&c, "").unwrap().is_empty());
    assert!(matches!(s.set(&c, "k", "v"), Err(SqlError::Sql(_))));
}

#[test]
fn joins_callers_transaction_and_rolls_back_with_it() {
    let mut c = conn();
    let s = plain();
    s.ensure_table(&c).unwrap();
    s.set(&c, "keep", "1").unwrap();
    {
        let tx = c.transaction().unwrap();
        s.set(&tx, "a", "1").unwrap();
        s.patch(&tx, &[("b", Some("2")), ("keep", None)]).unwrap();
        assert_eq!(s.get(&tx, "b").unwrap().as_deref(), Some("2"));
        // dropped without commit: everything rolls back
    }
    assert_eq!(s.get(&c, "a").unwrap(), None);
    assert_eq!(s.get(&c, "b").unwrap(), None);
    assert_eq!(s.get(&c, "keep").unwrap().as_deref(), Some("1"));
    {
        let tx = c.transaction().unwrap();
        s.set(&tx, "a", "1").unwrap();
        tx.commit().unwrap();
    }
    assert_eq!(s.get(&c, "a").unwrap().as_deref(), Some("1"));
}

#[test]
fn custom_table_and_columns_keep_names() {
    let c = conn();
    c.execute_batch("CREATE TABLE app_prefs (name TEXT PRIMARY KEY, val TEXT)")
        .unwrap();
    let s = plain().table("app_prefs").unwrap().columns("name", "val").unwrap();
    s.set(&c, "x", "y").unwrap();
    let got: String = c
        .query_row("SELECT val FROM app_prefs WHERE name = 'x'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(got, "y");
    assert!(matches!(
        plain().table("bad name;"),
        Err(SqlError::BadIdentifier(_))
    ));
}

fn rename_old(m: &mut serde_json::Map<String, Value>) -> Result<(), Error> {
    if let Some(v) = m.remove("old") {
        m.insert("new".into(), v);
    }
    Ok(())
}

#[test]
fn migrate_applies_pending_and_stamps_version() {
    let c = conn();
    let s = SqlSettings::new(Schema::new("mail", 2).migration(2, rename_old)).version_key("schema_version");
    s.ensure_table(&c).unwrap();
    s.set(&c, "old", "v").unwrap();
    s.set(&c, "other", "o").unwrap();
    assert_eq!(s.migrate(&c).unwrap(), 2);
    assert_eq!(s.get(&c, "old").unwrap(), None);
    assert_eq!(s.get(&c, "new").unwrap().as_deref(), Some("v"));
    assert_eq!(s.get(&c, "other").unwrap().as_deref(), Some("o"));
    assert_eq!(s.get(&c, "schema_version").unwrap().as_deref(), Some("2"));
    // already current: a second run changes nothing
    assert_eq!(s.migrate(&c).unwrap(), 2);
}

#[test]
fn newer_data_is_refused_and_never_changed() {
    let c = conn();
    let s = SqlSettings::new(Schema::new("mail", 1))
        .version_key("schema_version")
        .default_value("theme", "dark");
    s.ensure_table(&c).unwrap();
    c.execute_batch(
        "INSERT INTO settings VALUES ('schema_version','5'), ('theme','neon'), ('x','1')",
    )
    .unwrap();
    assert!(!s.writable(&c).unwrap());
    assert!(s.refusal(&c).unwrap().is_some());
    assert_eq!(s.get(&c, "theme").unwrap().as_deref(), Some("dark"));
    assert!(s.list(&c, "").unwrap().is_empty());
    for r in [
        s.set(&c, "x", "2"),
        s.remove(&c, "x").map(|_| ()),
        s.patch(&c, &[("x", None)]),
        s.migrate(&c).map(|_| ()),
    ] {
        assert!(matches!(
            r,
            Err(SqlError::Settings(Error::NewerSchema {
                found: 5,
                supported: 1
            }))
        ));
    }
    let n: i64 = c
        .query_row("SELECT count(*) FROM settings", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 3);
    let x: String = c
        .query_row("SELECT value FROM settings WHERE key='x'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(x, "1");
}

#[test]
fn failed_migration_writes_nothing() {
    fn boom(_: &mut serde_json::Map<String, Value>) -> Result<(), Error> {
        Err(Error::Invalid("no".into()))
    }
    let c = conn();
    let s = SqlSettings::new(Schema::new("mail", 1).migration(1, boom)).version_key("schema_version");
    s.ensure_table(&c).unwrap();
    s.set(&c, "a", "1").unwrap();
    assert!(s.migrate(&c).is_err());
    assert_eq!(s.get(&c, "schema_version").unwrap(), None);
    assert_eq!(s.get(&c, "a").unwrap().as_deref(), Some("1"));
}
