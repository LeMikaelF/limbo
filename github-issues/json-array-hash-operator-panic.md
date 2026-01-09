# JSON `$[#]` array length operator causes panic

## Summary

Using the `$[#]` path operator (which represents array length in SQLite's JSON path syntax) with any JSON function causes a panic.

## Reproducer

```sql
SELECT json_type('[1,2,3]', '$[#]');
```

Or any of these variations:
```sql
SELECT json_extract('[1,2]', '$[#]');
SELECT json_set('[1,2,3]', '$[#]', 4);
SELECT json_insert('[1,2,3]', '$[#]', 4);
SELECT json_replace('[1,2,3]', '$[#]', 4);
SELECT json_remove('[1,2,3]', '$[#]');
SELECT * FROM json_each('[1,2,3]', '$[#]');
SELECT * FROM json_tree('[1,2,3]', '$[#]');
```

## Error Message

```
thread 'main' panicked at core/json/jsonb.rs:2715:30:
internal error: entered unreachable code
```

## Root Cause

In `core/json/jsonb.rs`, the match arm for `ArrayLocator(idx)` only handles:
- `Some(idx)` when `idx >= 0`
- `Some(idx)` when `idx < 0`

But `$[#]` is parsed as `ArrayLocator(None)`, which falls through to `unreachable!()`.

## Expected Behavior

SQLite supports `$[#]` as a way to reference the element at the array length position (i.e., one past the last element), which is useful for appending. The implementation should either:
1. Support this operator properly (for functions like `json_insert`)
2. Return an appropriate error message instead of panicking
<!-- REPORTED -->
