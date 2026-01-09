# LIKE with long patterns causes panic due to regex size limit

## Summary

Using the `LIKE` operator with very long patterns causes a panic because the compiled regex exceeds the 10MB size limit.

## Reproducer

```sql
SELECT 'test' LIKE REPLACE(ZEROBLOB(135000), x'00', 'a');
```

Or with percent signs (triggers at smaller input due to `.*` expansion):
```sql
SELECT 'test' LIKE REPLACE(ZEROBLOB(11000), x'00', '%');
```

## Error Message

```
thread 'main' panicked at core/vdbe/value.rs:1132:10:
constructed LIKE regex pattern should be valid: CompiledTooBig(10485760)
```

## Root Cause

The `construct_like_regex` function at `core/vdbe/value.rs:1101-1133` converts LIKE patterns to regex patterns. Each alphabetic character becomes `[aA]` (4 characters) and `%` becomes `.*`. For very long patterns, the compiled regex exceeds the 10MB default limit.

The code uses `.expect()` on the regex build result:
```rust
RegexBuilder::new(&regex_pattern)
    .dot_matches_new_line(true)
    .build()
    .expect("constructed LIKE regex pattern should be valid")
```

## Expected Behavior

Instead of panicking, the code should:
1. Return an error (similar to how `construct_glob_regex` in `likeop.rs` handles errors gracefully), OR
2. Set a reasonable size limit on LIKE patterns to match SQLite's behavior

Note: The sister function `construct_glob_regex` in `core/vdbe/likeop.rs` correctly uses `map_err` to convert regex errors to `LimboError`, so GLOB handles the same patterns gracefully.
