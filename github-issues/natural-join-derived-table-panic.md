# Panic when NATURAL JOIN is used with derived tables

## Error

```
thread 'main' panicked at core/translate/planner.rs:1163:51:
column name is None
```

## Reproducer

```sql
SELECT t.* FROM (SELECT 1) t NATURAL JOIN (SELECT 1) u
```

## Command

```bash
cargo run --bin tursodb -q -- -q -m list :memory: "SELECT t.* FROM (SELECT 1) t NATURAL JOIN (SELECT 1) u"
```

## Analysis

The panic occurs in the query planner when processing a NATURAL JOIN between two derived tables (subqueries in the FROM clause). The issue is that derived tables from `SELECT` without explicit column aliases produce columns without names, and the NATURAL JOIN logic at `planner.rs:1163` assumes all columns have names when trying to match columns for the implicit join condition.

The code at line 1163 does:
```rust
col.name.as_ref().unwrap()
```

This unwraps `None` when the column comes from an anonymous derived table expression like `(SELECT 1)` which doesn't have an explicit column name.
<!-- REPORTED -->
