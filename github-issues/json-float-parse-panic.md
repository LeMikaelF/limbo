# Panic when extracting malformed JSONB float values

## Summary

Providing a malformed JSONB blob with a FLOAT type tag but unparseable content causes a panic in `json_extract()` and other JSON functions.

## Reproducer

```sql
SELECT json_extract(X'256162', '$');
```

Additional reproducers:
```sql
-- Using malformed float string '1.e+'
SELECT json_extract(X'45312e652b', '$');

-- Using malformed float string '1..1'
SELECT json_extract(X'35312e2e31', '$');

-- Via json_insert
SELECT json_insert(X'45312e652b', '$.a', 1);

-- Via json_set
SELECT json_set(X'45312e652b', '$.x', 3);

-- Using FLOAT5 type (type=6)
SELECT json_extract(X'266162', '$');
```

## Error Message

```
thread 'main' panicked at core/json/mod.rs:560:54:
Should be valid f64: ParseFloatError { kind: Invalid }
```

## Root Cause

In `core/json/mod.rs:560`, the code assumes that any JSONB value tagged as FLOAT can be parsed as an f64:

```rust
ElementType::FLOAT5 | ElementType::FLOAT => {
    let float_val: f64 = json_string.parse().expect("Should be valid f64");
```

However, JSONB blobs can be crafted directly using hex literals (`X'...'`), allowing an attacker to create a blob with a FLOAT type tag (5 or 6) but with arbitrary byte content that cannot be parsed as a valid floating-point number.

## JSONB Blob Format

The malformed blobs use this format:
- Header byte: `(payload_size << 4) | element_type`
- FLOAT type = 5, FLOAT5 type = 6
- Example: `X'256162'` = header `0x25` (size=2, type=5) + data `"ab"` (0x61 0x62)

## Impact

This is a denial-of-service vulnerability. An attacker can:
1. Insert malformed JSONB blobs directly using hex literals
2. Store them in BLOB columns
3. Trigger the panic when any JSON extraction function processes them

## Suggested Fix

Replace the `expect()` with proper error handling that returns NULL or an error for invalid float values, similar to how invalid JSON input is handled elsewhere in the codebase.
<!-- REPORTED -->
