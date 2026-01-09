# Panic when PRAGMA query_only is usec with non-integer numeric literals

## Summary

Setting `PRAGMA query_only` with floating-point, scientific notation, hexadecimal, or overflow integer values causes a panic instead of being handled gracefully.

## Reproducer

Any of the following SQL statements trigger the panic:

```sql
-- Scientific notation
PRAGMA query_only=1e10;

-- Floating point
PRAGMA query_only=1.5;

-- Hexadecimal
PRAGMA query_only=0xFF;

-- Integer overflow (i64::MAX + 1)
PRAGMA query_only=9223372036854775808;
```

### Minimal reproduction command:

```bash
cargo run --bin tursodb -q -- -q :memory: "PRAGMA query_only=1e10"
```

## Error Message

```
thread 'main' panicked at core/translate/pragma.rs:697:66:
called `Result::unwrap()` on an `Err` value: ParseIntError { kind: InvalidDigit }
```

Or for overflow:
```
thread 'main' panicked at core/translate/pragma.rs:697:66:
called `Result::unwrap()` on an `Err` value: ParseIntError { kind: PosOverflow }
```

## Expected Behavior

SQLite handles all these inputs gracefully. For example:
- `PRAGMA query_only=1e10` sets query_only to 1 in SQLite
- `PRAGMA query_only=1.5` sets query_only to 1 in SQLite
- `PRAGMA query_only=0xFF` sets query_only to 1 in SQLite

Turso should either:
1. Parse the value as a float first and convert to bool (non-zero = true), or
2. Return a meaningful error message instead of panicking

## Root Cause

In `core/translate/pragma.rs:697`, the code attempts to parse a `Literal::Numeric` string directly as `i64`:

```rust
ast::Expr::Literal(Literal::Numeric(i)) => i.parse::<i64>().unwrap() != 0,
```

The `Literal::Numeric` variant can contain valid SQL numeric literals that are not valid i64 integers (floats, scientific notation, hex, numbers > i64::MAX).

## Affected Inputs

| Input Type | Example | ParseIntError |
|------------|---------|---------------|
| Scientific notation | `1e10` | InvalidDigit |
| Floating point | `1.5` | InvalidDigit |
| Hexadecimal | `0xFF` | InvalidDigit |
| Underscore separator | `1_000` | InvalidDigit |
| Positive overflow | `9223372036854775808` | PosOverflow |
