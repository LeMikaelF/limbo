# Panic: Zero parameter index (?0) causes ParseIntError panic

## Summary

Using `?0` as a parameter index in SQL statements causes a panic in `core/parameters.rs:102`. SQLite rejects `?0` with a proper error message, but Turso panics instead.

## Reproducer

```bash
cargo run --bin tursodb -q -- -q -m list :memory: "SELECT ?0"
```

## Error Message

```
thread 'main' panicked at core/parameters.rs:102:59:
called `Result::unwrap()` on an `Err` value: ParseIntError { kind: Zero }
```

## Root Cause

The parser in `parser/src/parser.rs` validates positional parameter indices using `parse::<u32>()` (line 189), which accepts "0" as valid. However, `core/parameters.rs` then tries to parse the same string as `NonZero<usize>` (line 102), which panics because "0" cannot be parsed as a non-zero value.

The code flow:
1. SQL `SELECT ?0` is lexed, producing token with value `"0"` (the `?` is stripped)
2. Parser's `create_variable()` validates `"0".parse::<u32>()` which succeeds (0 is valid u32)
3. `Expr::Variable("0")` is created
4. In translation, `parameters.push("0")` is called
5. `"0".parse::<NonZero<usize>>()` fails with `ParseIntError { kind: Zero }`
6. `.unwrap()` panics

## Expected Behavior

SQLite rejects `?0` with a proper error message:
```
Error: in prepare, variable number must be between ?1 and ?250000
```

Turso should return a similar error instead of panicking.

## Additional SQL Statements That Trigger the Same Panic

- `SELECT ?0`
- `SELECT ?00`
- `SELECT ?0 + 1`
- `SELECT coalesce(?0, 1)`
- `SELECT ABS(?0)`
- `CREATE TABLE t(a); INSERT INTO t VALUES(?0)`
- Any SQL statement using `?0` as a parameter placeholder

## Suggested Fix

The fix should be in `/workspace/parser/src/parser.rs` in the `create_variable` function. After parsing as `u32`, reject zero:

```rust
let variable_id = variable_str
    .parse::<u32>()
    .map_err(|e| Error::Custom(format!("non-integer positional variable id: {e}")))?;
if variable_id == 0 {
    return Err(Error::Custom("variable number must be between ?1 and ?250000".to_string()));
}
```
