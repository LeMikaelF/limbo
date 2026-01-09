# uuid7_timestamp_ms panics on non-16-byte blobs

## Summary

The `uuid7_timestamp_ms()` function panics when passed a blob that is not exactly 16 bytes, instead of returning NULL or an error.

## Reproducer

```sql
select uuid7_timestamp_ms(X'')
```

Or any blob that is not exactly 16 bytes:
```sql
select uuid7_timestamp_ms(X'00')
select uuid7_timestamp_ms(randomblob(5))
select uuid7_timestamp_ms(zeroblob(10))
```

## Error Message

```
thread 'main' panicked at core/uuid.rs:95:64:
called `Result::unwrap()` on an `Err` value: Error(ByteLength { len: 0 })
```

## Root Cause

In `core/uuid.rs:95`, the code calls `.unwrap()` on the result of `uuid::Uuid::from_slice()`:

```rust
let uuid = uuid::Uuid::from_slice(blob.as_slice()).unwrap();
```

The `uuid::Uuid::from_slice()` function requires exactly 16 bytes and returns an error for any other length. The `.unwrap()` causes a panic instead of gracefully handling the error.

## Expected Behavior

The function should return NULL for invalid input, similar to how the text parsing path handles invalid UUIDs:

```rust
ValueType::Text => {
    // ...
    let Ok(uuid) = uuid::Uuid::parse_str(text) else {
        return Value::null();  // Graceful handling
    };
}
```
