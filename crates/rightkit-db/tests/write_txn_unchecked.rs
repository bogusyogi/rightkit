//! `write_txn_unchecked` takes the writer slot from a shared borrow and rolls back unless
//! committed. Real in-memory connection.

use rightkit_db::{open_in_memory, write_txn_unchecked};

fn count(conn: &rightkit_db::rusqlite::Connection) -> i64 {
    conn.query_row("SELECT count(*) FROM t", [], |row| row.get(0))
        .unwrap()
}

#[test]
fn an_uncommitted_transaction_rolls_back_and_a_committed_one_persists() {
    let conn = open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE t(v INTEGER);").unwrap();
    {
        let tx = write_txn_unchecked(&conn).unwrap();
        tx.execute("INSERT INTO t VALUES (1)", []).unwrap();
    }
    assert_eq!(count(&conn), 0);
    {
        let tx = write_txn_unchecked(&conn).unwrap();
        tx.execute("INSERT INTO t VALUES (2)", []).unwrap();
        tx.commit().unwrap();
    }
    assert_eq!(count(&conn), 1);
}

#[test]
fn a_nested_unchecked_transaction_is_refused_by_sqlite() {
    let conn = open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE t(v INTEGER);").unwrap();
    let outer = write_txn_unchecked(&conn).unwrap();
    assert!(write_txn_unchecked(&conn).is_err());
    outer.commit().unwrap();
}
