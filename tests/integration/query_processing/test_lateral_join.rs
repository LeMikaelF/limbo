use crate::common::{ExecRows, TempDatabase};
use turso_core::{LimboError, Value};

// Test basic LATERAL join functionality
#[turso_macros::test(mvcc)]
fn test_lateral_join_basic(tmp_db: TempDatabase) -> anyhow::Result<()> {
    let conn = tmp_db.connect_limbo();

    // Setup tables
    conn.execute("CREATE TABLE t1 (id INTEGER PRIMARY KEY, val INTEGER)")?;
    conn.execute("CREATE TABLE t2 (id INTEGER PRIMARY KEY, t1_id INTEGER, val INTEGER)")?;
    conn.execute("INSERT INTO t1 VALUES (1, 10), (2, 20), (3, 30)")?;
    conn.execute(
        "INSERT INTO t2 VALUES (1, 1, 100), (2, 1, 101), (3, 2, 200), (4, 3, 300), (5, 3, 301)",
    )?;

    // LATERAL join allows the subquery to reference columns from preceding tables
    // This query finds for each row in t1, the first matching row in t2
    let rows: Vec<(i64, i64, i64)> = conn.exec_rows(
        "SELECT t1.id, t1.val, sub.t2_val
         FROM t1
         JOIN LATERAL (SELECT t2.val as t2_val FROM t2 WHERE t2.t1_id = t1.id LIMIT 1) AS sub ON true
         ORDER BY t1.id",
    );

    assert_eq!(rows.len(), 3);
    // t1.id=1 has t2 rows 100, 101 -> first is 100
    assert_eq!(rows[0], (1, 10, 100));
    // t1.id=2 has t2 row 200
    assert_eq!(rows[1], (2, 20, 200));
    // t1.id=3 has t2 rows 300, 301 -> first is 300
    assert_eq!(rows[2], (3, 30, 300));

    Ok(())
}

// Test LATERAL join with multiple rows from subquery
#[turso_macros::test(mvcc)]
fn test_lateral_join_multiple_rows(tmp_db: TempDatabase) -> anyhow::Result<()> {
    let conn = tmp_db.connect_limbo();

    // Setup tables
    conn.execute("CREATE TABLE t1 (id INTEGER PRIMARY KEY, val INTEGER)")?;
    conn.execute("CREATE TABLE t2 (id INTEGER PRIMARY KEY, t1_id INTEGER, val INTEGER)")?;
    conn.execute("INSERT INTO t1 VALUES (1, 10), (2, 20)")?;
    conn.execute("INSERT INTO t2 VALUES (1, 1, 100), (2, 1, 101), (3, 2, 200)")?;

    // LATERAL subquery returns multiple rows per outer row
    let rows: Vec<(i64, i64)> = conn.exec_rows(
        "SELECT t1.id, sub.t2_val
         FROM t1
         JOIN LATERAL (SELECT t2.val as t2_val FROM t2 WHERE t2.t1_id = t1.id) AS sub ON true
         ORDER BY t1.id, sub.t2_val",
    );

    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0], (1, 100)); // t1.id=1, first t2 match
    assert_eq!(rows[1], (1, 101)); // t1.id=1, second t2 match
    assert_eq!(rows[2], (2, 200)); // t1.id=2, only t2 match

    Ok(())
}

// Test LEFT JOIN LATERAL
#[turso_macros::test(mvcc)]
fn test_left_lateral_join(tmp_db: TempDatabase) -> anyhow::Result<()> {
    let conn = tmp_db.connect_limbo();

    // Setup tables
    conn.execute("CREATE TABLE t1 (id INTEGER PRIMARY KEY, val INTEGER)")?;
    conn.execute("CREATE TABLE t2 (id INTEGER PRIMARY KEY, t1_id INTEGER, val INTEGER)")?;
    conn.execute("INSERT INTO t1 VALUES (1, 10), (2, 20), (3, 30)")?;
    conn.execute("INSERT INTO t2 VALUES (1, 1, 100), (2, 2, 200)")?;

    // LEFT JOIN LATERAL should include rows where subquery returns no matches
    let mut stmt = conn.prepare(
        "SELECT t1.id, t1.val, sub.t2_val
         FROM t1
         LEFT JOIN LATERAL (SELECT t2.val as t2_val FROM t2 WHERE t2.t1_id = t1.id LIMIT 1) AS sub ON true
         ORDER BY t1.id",
    )?;

    let mut results = Vec::new();
    stmt.run_with_row_callback(|row| {
        let id: i64 = row.get(0)?;
        let val: i64 = row.get(1)?;
        let t2_val: &Value = row.get(2)?;
        results.push((id, val, t2_val.clone()));
        Ok(())
    })?;

    assert_eq!(results.len(), 3);
    assert_eq!(results[0], (1, 10, Value::Integer(100)));
    assert_eq!(results[1], (2, 20, Value::Integer(200)));
    assert_eq!(results[2], (3, 30, Value::Null)); // No match for t1.id=3

    Ok(())
}

// Test comma-style LATERAL join (implicit cross join)
#[turso_macros::test(mvcc)]
fn test_comma_lateral_join(tmp_db: TempDatabase) -> anyhow::Result<()> {
    let conn = tmp_db.connect_limbo();

    // Setup tables
    conn.execute("CREATE TABLE t1 (id INTEGER PRIMARY KEY, val INTEGER)")?;
    conn.execute("CREATE TABLE t2 (id INTEGER PRIMARY KEY, t1_id INTEGER, val INTEGER)")?;
    conn.execute("INSERT INTO t1 VALUES (1, 10), (2, 20)")?;
    conn.execute("INSERT INTO t2 VALUES (1, 1, 100), (2, 1, 101), (3, 2, 200)")?;

    // Comma with LATERAL is like CROSS JOIN LATERAL
    let rows: Vec<(i64, i64)> = conn.exec_rows(
        "SELECT t1.id, sub.t2_val
         FROM t1, LATERAL (SELECT t2.val as t2_val FROM t2 WHERE t2.t1_id = t1.id) AS sub
         ORDER BY t1.id, sub.t2_val",
    );

    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0], (1, 100));
    assert_eq!(rows[1], (1, 101));
    assert_eq!(rows[2], (2, 200));

    Ok(())
}

// Test LATERAL with aggregation in subquery
#[turso_macros::test(mvcc)]
fn test_lateral_join_with_aggregation(tmp_db: TempDatabase) -> anyhow::Result<()> {
    let conn = tmp_db.connect_limbo();

    // Setup tables
    conn.execute("CREATE TABLE t1 (id INTEGER PRIMARY KEY, val INTEGER)")?;
    conn.execute("CREATE TABLE t2 (id INTEGER PRIMARY KEY, t1_id INTEGER, val INTEGER)")?;
    conn.execute("INSERT INTO t1 VALUES (1, 10), (2, 20), (3, 30)")?;
    conn.execute(
        "INSERT INTO t2 VALUES (1, 1, 100), (2, 1, 150), (3, 2, 200), (4, 3, 300), (5, 3, 350)",
    )?;

    // LATERAL subquery with aggregation
    let rows: Vec<(i64, i64, i64)> = conn.exec_rows(
        "SELECT t1.id, t1.val, sub.sum_val
         FROM t1
         JOIN LATERAL (SELECT SUM(t2.val) as sum_val FROM t2 WHERE t2.t1_id = t1.id) AS sub ON true
         ORDER BY t1.id",
    );

    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0], (1, 10, 250)); // 100 + 150 = 250
    assert_eq!(rows[1], (2, 20, 200)); // 200
    assert_eq!(rows[2], (3, 30, 650)); // 300 + 350 = 650

    Ok(())
}

// Test error case: LATERAL with table reference instead of subquery
#[turso_macros::test(mvcc)]
fn test_lateral_with_table_reference_error(tmp_db: TempDatabase) -> anyhow::Result<()> {
    let conn = tmp_db.connect_limbo();

    // Setup tables
    conn.execute("CREATE TABLE t1 (id INTEGER PRIMARY KEY, val INTEGER)")?;
    conn.execute("CREATE TABLE t2 (id INTEGER PRIMARY KEY, t1_id INTEGER)")?;

    // LATERAL can only be used with subqueries, not table references
    let result = conn.prepare("SELECT * FROM t1 JOIN LATERAL t2 ON t1.id = t2.t1_id");

    assert!(result.is_err());
    let err = result.unwrap_err();
    // Parser errors are converted to LimboError::LexerError
    assert!(matches!(err, LimboError::LexerError(_)));

    Ok(())
}

// Test LATERAL join order is preserved (not reordered by optimizer)
#[turso_macros::test(mvcc)]
fn test_lateral_join_order_preserved(tmp_db: TempDatabase) -> anyhow::Result<()> {
    let conn = tmp_db.connect_limbo();

    // Setup tables
    conn.execute("CREATE TABLE t1 (id INTEGER PRIMARY KEY, val INTEGER)")?;
    conn.execute("CREATE TABLE t2 (id INTEGER PRIMARY KEY, t1_id INTEGER, val INTEGER)")?;
    conn.execute("CREATE TABLE t3 (id INTEGER PRIMARY KEY, t2_id INTEGER, val INTEGER)")?;
    conn.execute("INSERT INTO t1 VALUES (1, 10), (2, 20)")?;
    conn.execute("INSERT INTO t2 VALUES (1, 1, 100), (2, 2, 200)")?;
    conn.execute("INSERT INTO t3 VALUES (1, 1, 1000), (2, 2, 2000)")?;

    // Multi-table join with LATERAL - order must be preserved
    let rows: Vec<(i64, i64, i64)> = conn.exec_rows(
        "SELECT t1.id, sub1.t2_val, sub2.t3_val
         FROM t1
         JOIN LATERAL (SELECT t2.id as t2_id, t2.val as t2_val FROM t2 WHERE t2.t1_id = t1.id) AS sub1 ON true
         JOIN LATERAL (SELECT t3.val as t3_val FROM t3 WHERE t3.t2_id = sub1.t2_id) AS sub2 ON true
         ORDER BY t1.id",
    );

    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0], (1, 100, 1000));
    assert_eq!(rows[1], (2, 200, 2000));

    Ok(())
}

// Test LATERAL with LIMIT in subquery (top-N pattern)
#[turso_macros::test(mvcc)]
fn test_lateral_top_n_pattern(tmp_db: TempDatabase) -> anyhow::Result<()> {
    let conn = tmp_db.connect_limbo();

    // Setup tables
    conn.execute("CREATE TABLE products (id INTEGER PRIMARY KEY, name TEXT)")?;
    conn.execute(
        "CREATE TABLE sales (id INTEGER PRIMARY KEY, product_id INTEGER, amount INTEGER, sale_date TEXT)",
    )?;
    conn.execute("INSERT INTO products VALUES (1, 'Widget'), (2, 'Gadget')")?;
    conn.execute(
        "INSERT INTO sales VALUES (1, 1, 100, '2024-01-01'), (2, 1, 150, '2024-01-02'), (3, 1, 75, '2024-01-03')",
    )?;
    conn.execute("INSERT INTO sales VALUES (4, 2, 200, '2024-01-01'), (5, 2, 180, '2024-01-02')")?;

    // Top-2 sales per product using LATERAL
    let rows: Vec<(i64, String, i64)> = conn.exec_rows(
        "SELECT p.id, p.name, s.amount
         FROM products p
         JOIN LATERAL (
             SELECT amount FROM sales WHERE product_id = p.id ORDER BY amount DESC LIMIT 2
         ) AS s ON true
         ORDER BY p.id, s.amount DESC",
    );

    assert_eq!(rows.len(), 4);
    // Product 1: top 2 are 150, 100
    assert_eq!(rows[0], (1, "Widget".to_string(), 150));
    assert_eq!(rows[1], (1, "Widget".to_string(), 100));
    // Product 2: top 2 are 200, 180
    assert_eq!(rows[2], (2, "Gadget".to_string(), 200));
    assert_eq!(rows[3], (2, "Gadget".to_string(), 180));

    Ok(())
}
