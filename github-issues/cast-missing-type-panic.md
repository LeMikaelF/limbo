# Panic when CAST is used without type

## Summary

Using `CAST(expr AS)` syntax (with `AS` keyword but no type name) causes a panic in the query translator.

## Reproducer

```sql
SELECT CAST(1 AS);
```

Run with:
```bash
cargo run --bin tursodb -q -- -q -m list :memory: "SELECT CAST(1 AS)"
```

## Error Message

```
thread 'main' panicked at core/translate/expr.rs:965:48:
called `Option::unwrap()` on a `None` value
```

## Root Cause

The `Cast` AST node in `/workspace/parser/src/ast.rs` defines `type_name` as `Option<Type>`:

```rust
Cast {
    expr: Box<Expr>,
    type_name: Option<Type>,
},
```

The parser's `parse_type()` function returns `None` when the next token after `AS` is not an identifier or string. This allows SQL like `CAST(1 AS)` to be parsed successfully with `type_name = None`, but then the translator panics at `core/translate/expr.rs:965`:

```rust
let type_name = type_name.as_ref().unwrap(); // TODO: why is this optional?
```

## Additional Reproducing Statements

All of the following also trigger the panic:
- `SELECT CAST('test' AS)`
- `SELECT CAST(NULL AS)`
- `SELECT CAST(1+2 AS)`
- `SELECT 1 WHERE CAST(1 AS) = 1`
- `SELECT CAST((SELECT 1) AS)`
- `SELECT CAST(CAST(1 AS) AS INTEGER)` (nested)
- `SELECT COALESCE(CAST(1 AS), 0)`
- `SELECT CASE WHEN 1 THEN CAST(1 AS) END`

## SQLite Behavior

For reference, SQLite accepts `CAST(x AS)` as valid SQL and treats it as casting to NUMERIC affinity:
- `SELECT CAST(1 AS)` returns `1`
- `SELECT CAST('hello' AS)` returns `0`

## Suggested Fix

Either:
1. **Reject in parser**: Modify `parse_type()` to return an error when called from CAST context and no type is provided
2. **Handle in translator**: Replace the `unwrap()` with proper handling - either return a parse error or apply NUMERIC affinity (to match SQLite behavior)
<!-- REPORTED -->
