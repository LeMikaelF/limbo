# Panic in datetime functions on multi-byte UTF-8 characters

## Summary

The datetime functions (`datetime()`, `date()`, `time()`, `julianday()`) panic when processing strings containing multi-byte UTF-8 characters (emojis, Chinese, Japanese, Russian, Greek, etc.).

## Reproducer

```sql
select datetime('2024-01-01 🎉🎉')
```

Or with other multi-byte characters:
```sql
select date('2024-01-01 中文字')
select time('2024-01-01 日本語')
select julianday('2024-01-01 Привет')
select datetime('2024-01-01 αβγδεζ')
```

## Error Message

```
thread 'main' panicked at core/functions/datetime.rs:997:36:
called `Option::unwrap()` on a `None` value
```

## Root Cause

In `core/functions/datetime.rs:995-997`, the code uses byte length to index into characters:

```rust
if s.len() >= 6 {
    let idx = s.len() - 6;
    let c = s.chars().nth(idx).unwrap();  // PANIC HERE
```

The issue is that `s.len()` returns the **byte length**, but `s.chars().nth(idx)` iterates by **Unicode code points**. For multi-byte UTF-8 characters, the byte length can be much larger than the character count.

**Example:** The string `🎉🎉` has:
- Byte length: 8 (each emoji is 4 bytes)
- Character count: 2

When processing with the time portion `🎉🎉`:
- `s.len()` returns 8
- `idx = 8 - 6 = 2`
- `s.chars().nth(2)` returns `None` because there are only 2 characters (at indices 0 and 1)
- `.unwrap()` panics

## Expected Behavior

The function should gracefully handle multi-byte UTF-8 strings, either by:
1. Using byte indexing consistently (and handling potential invalid UTF-8 boundaries)
2. Using character count instead of byte length for the bounds check
3. Using `.get()` with a fallback instead of `.unwrap()`
<!-- REPORTED -->
