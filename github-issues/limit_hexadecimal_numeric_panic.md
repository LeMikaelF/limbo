# PANIC when LIMIT clause has hexadecimal and underscore numeric literals

## Summary

The SQL code generator panics when a LIMIT clause contains a hexadecimal literal (e.g., `0x10`) or a numeric literal with underscores (e.g., `1_000`). The parser accepts these formats, but the emitter fails to parse them as f64.

## Reproducer

```sql
SELECT 1 LIMIT 0x10
```

Or with underscore notation:
```sql
SELECT 1 LIMIT 1_2_3
```

## Error Message

```
thread 'main' panicked at core/translate/emitter.rs:3361:53:
called `Result::unwrap()` on an `Err` value: ParseFloatError { kind: Invalid }
```

## Root Cause

In `core/translate/emitter.rs:3361`, when a LIMIT value cannot be parsed as `i64`, the code falls back to parsing as `f64` with an `.unwrap()`:

```rust
if let Ok(value) = n.parse::<i64>() {
    // ...integer path
} else {
    program.emit_insn(Insn::Real {
        value: n.parse::<f64>().unwrap(),  // PANICS HERE
        dest: limit_ctx.reg_limit,
    });
}
```

The parser (`Literal::Numeric`) accepts numeric literals in formats that Rust's `parse::<f64>()` cannot handle:
- Hexadecimal: `0x10`, `0xFF`
- Underscores: `1_000`, `1_2_3`

When `parse::<i64>()` fails (because hex/underscore strings fail i64 parsing too), the fallback to `parse::<f64>()` also fails, causing the panic.

## Interesting Note

The OFFSET clause handling at line 3392 uses the `?` operator instead of `.unwrap()`:
```rust
let value = n.parse::<f64>()?;  // Graceful error handling
```

This inconsistency suggests the LIMIT case was an oversight.

## Expected Behavior

Either:
1. Use `?` operator like the OFFSET case to propagate the error gracefully
2. Add proper handling for hex/underscore numeric formats before falling back to f64 parsing
3. Reject these formats at the parser level if they shouldn't be supported in LIMIT

## Additional Reproducers

```sql
-- Hex with letters
SELECT 1 LIMIT 0xFF

-- OFFSET works but LIMIT panics for the same input
SELECT 1 LIMIT 10 OFFSET 0x5  -- Would error gracefully at OFFSET
SELECT 1 LIMIT 0x5 OFFSET 10  -- Panics at LIMIT
```
<!-- REPORTED -->
