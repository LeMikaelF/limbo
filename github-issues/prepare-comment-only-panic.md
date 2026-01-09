# Panic in `prepare()` with comment-only or whitespace-only SQL

## Summary

Calling `prepare()` with SQL that contains only comments, whitespace, or semicolons causes a panic. The parser returns `Ok(None)` for these inputs, but the code expects a command and panics on `.expect()`.

## Panic Location

**File**: `core/lib.rs:1383`

```rust
let cmd = cmd.expect("Successful parse on nonempty input string should produce a command");
```

## Error Message

```
thread 'main' panicked at core/lib.rs:1383:19:
Successful parse on nonempty input string should produce a command
```

## Reproduction

Any of these inputs trigger the panic:

```sql
-- Single semicolon
;

-- Block comment only
/* comment */

-- Line comment only
-- this is a comment

-- Whitespace only



-- Multiple semicolons
;;;

-- Empty block comment
/**/

-- Nested-looking comment (not actually nested in SQL)
/* outer /* inner */ still outer */
```

### CLI Commands

```bash
cargo run --bin tursodb -q -- -q -m list :memory: ";"
cargo run --bin tursodb -q -- -q -m list :memory: "/* comment */"
cargo run --bin tursodb -q -- -q -m list :memory: "-- comment"
```

## Root Cause Analysis

The parser (`Parser::next_cmd()`) returns `Ok(None)` when parsing succeeds but produces no command. This happens for:

1. Empty input after whitespace trimming
2. Comment-only input (both `--` and `/* */` styles)
3. Semicolon-only input (empty statements)

The `prepare()` function at `lib.rs:1383` assumes that any non-empty input string will produce a command, which is incorrect. The `.expect()` call panics when the parser returns `Ok(None)`.

## Suggested Fix

Handle the `None` case explicitly by returning an empty/no-op statement or an appropriate error:

```rust
// Option 1: Return error for empty statements
let cmd = match cmd {
    Some(cmd) => cmd,
    None => return Err(LimboError::ParseError("Empty or comment-only SQL".to_string())),
};

// Option 2: Return a no-op statement (if supported)
let cmd = match cmd {
    Some(cmd) => cmd,
    None => return Ok(Statement::empty()),
};
```

## Impact

- **Severity**: Medium - causes crash on user input
- **User-facing**: Yes - can be triggered via CLI or API
- **SQLite compatibility**: SQLite handles these inputs gracefully without panicking

## SQLite Behavior

SQLite returns an empty result for these inputs without error:
```bash
$ sqlite3 :memory: ";"
$ sqlite3 :memory: "/* comment */"
$ sqlite3 :memory: "-- comment"
```

## Related

This is similar to other input validation panics tracked in the panic index.
