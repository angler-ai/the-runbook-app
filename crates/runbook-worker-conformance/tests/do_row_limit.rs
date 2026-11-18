//! CF12b (CF12-8's open item): Durable Object SQLite holds at most
//! 2,000,000 bytes in a string, a BLOB or a row, as D1 does, and the native
//! stand-in (`NativeSql`) now refuses past it too, so a native test fails
//! where an ExecutionDO would.
use runbook_store_do::{DoSql, Value};
use runbook_worker_conformance::native::NativeSql;

#[test]
fn values_and_rows_are_held_to_the_objects_limit() {
    let sql = NativeSql::new(0);
    sql.exec_script("CREATE TABLE t (a TEXT, b BLOB)").unwrap();
    let insert = |a: usize, b: usize| {
        sql.exec(
            "INSERT INTO t (a, b) VALUES (?, ?)",
            &[Value::Text("x".repeat(a)), Value::Blob(vec![0; b])],
        )
    };
    assert!(insert(1_999_000, 0).is_ok(), "under the limit");
    assert!(insert(0, 1_999_000).is_ok(), "a BLOB under the limit");
    assert!(insert(2_000_001, 0).is_err(), "one string over");
    assert!(insert(0, 2_000_001).is_err(), "one BLOB over");
    assert!(insert(1_000_000, 1_000_000).is_err(), "a row over");
    let rows = sql.query("SELECT count(*) FROM t", &[]).unwrap();
    assert_eq!(rows[0][0], Value::Integer(2));
}
