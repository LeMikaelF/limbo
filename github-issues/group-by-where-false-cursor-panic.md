# GROUP BY with WHERE 0 causes cursor id panic

## Reproducer

```sql
SELECT 1 WHERE 0 GROUP BY 1
```

Run with:
```bash
cargo run --bin tursodb -q -- -q -m list :memory: "SELECT 1 WHERE 0 GROUP BY 1"
```

## Error Message

```
thread 'main' panicked at core/vdbe/mod.rs:567:32:
cursor id 0 is None
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace
```

## Analysis

When a SELECT with GROUP BY has a WHERE clause that evaluates to constant false (`WHERE 0`), the cursor for the sorter is not properly initialized before being accessed. The VM tries to access cursor 0 but it's None.

This appears to be an issue in how the code generation handles the combination of:
1. GROUP BY clause (which needs a sorter cursor)
2. Constant false WHERE condition (which may cause early optimization/skip of cursor setup)

The panic occurs because the bytecode assumes the cursor was opened, but the constant false condition caused the cursor initialization to be skipped.

## Additional Notes

Similar queries that also exhibit issues:
- `SELECT 1 GROUP BY 1` - causes infinite loop (timeout)
- `SELECT NULL GROUP BY 1` - causes infinite loop (timeout)
- Views with GROUP BY on literal values - causes infinite loop
