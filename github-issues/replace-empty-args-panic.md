# Panic when replace() function has no arguments

## Reproducer

```sql
SELECT replace();
```

## Error

```
thread 'main' panicked at core/translate/expr.rs:1874:17:
index out of bounds: the len is 0 but the index is 0
```

## Root Cause

The guard condition at line 1861 has an operator precedence bug:

```rust
if !args.len() == 3 {
    crate::bail_parse_error!("replace() requires 3 arguments");
}
```

The `!` operator has higher precedence than `==`, so `!args.len()` performs a bitwise NOT on the length (e.g., `!0` = `usize::MAX`), which will never equal 3. The correct code should be:

```rust
if args.len() != 3 {
    crate::bail_parse_error!("replace() requires 3 arguments");
}
```

Because the guard never triggers, the function proceeds to access `&args[0]` at line 1874, which panics when args is empty.

## Expected Behavior

SQLite returns an error: `wrong number of arguments to function replace()`
