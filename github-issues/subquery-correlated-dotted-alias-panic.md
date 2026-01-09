# Panic: "Subquery result_columns_start_reg must be set"

## Summary
A panic occurs in `core/translate/expr.rs:2349:30` when using a correlated subquery that references a derived table with a dotted alias name.

## Panic Message
```
thread 'main' panicked at core/translate/expr.rs:2349:30:
Subquery result_columns_start_reg must be set
```

## Reproducer
```sql
SELECT * FROM (SELECT 1 as x) AS "a.b" WHERE EXISTS (SELECT "a.b".x)
```

### Minimal reproducer variations:
```sql
-- With quoted dotted alias
SELECT * FROM (SELECT 1 as x) AS "." WHERE EXISTS (SELECT ".".x)

-- With different dotted name
SELECT * FROM (SELECT 1 as x) AS "foo.bar" WHERE EXISTS (SELECT "foo.bar".x)
```

## Steps to Reproduce
```bash
cargo run --bin tursodb -- :memory: 'SELECT * FROM (SELECT 1 as x) AS "a.b" WHERE EXISTS (SELECT "a.b".x)'
```

## Expected Behavior
The query should either:
1. Execute successfully and return the row (if the correlated reference is valid), or
2. Return a proper parse/semantic error if the reference is invalid

## Actual Behavior
The database panics with an assertion failure indicating that `result_columns_start_reg` was not set for a subquery.

## Root Cause Analysis
The issue appears to be in how correlated subqueries resolve references to outer tables when the outer table has a dotted alias name. The dotted alias (e.g., `"a.b"`) is likely being misinterpreted as a database-qualified table reference (`database.table`) rather than a quoted identifier.

When the correlated EXISTS subquery tries to reference `"a.b".x`, the translation layer fails to properly set up the subquery's result column registers because the outer reference resolution goes down an unexpected code path.

## Affected Code
- `core/translate/expr.rs:2349` - The expect() that triggers the panic
- Likely related to table/column resolution for dotted identifiers in `core/translate/logical.rs`

## Workaround
Avoid using alias names that contain dots when the derived table will be referenced in a correlated subquery.

## Environment
- Turso/Limbo SQLite implementation
- Found during panic investigation
<!-- REPORTED -->
