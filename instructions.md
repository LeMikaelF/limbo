[//]: # (TODO improvements to this: modify the script to tag files "REPORTED", and ignore tagged files)

# Hunting unwraps!

This codebase makes extensive use of `unwrap()`, but this makes the code prone to panics. We have already discovered the
following bugs, all of which are reproducible using the CLI and plain SQL statements:

```
thread 'main' panicked at core/types.rs:2318:51:
called `Option::unwrap()` on a `None` value


thread 'main' panicked at core/types.rs:1919:32:
called `Option::unwrap()` on a `None` value

thread 'main' panicked at core/types.rs:1752:9:
called `Option::unwrap()` on a `None` value
```

We know there must be many more such cases involving `Option::unwrap` or similar panicking methods.

# Your task

Your task is to find and reproduce as many of these panics as possible by using just the CLI and SQL statements. You can
use the CLI like so:

```
cargo run --bin tursodb -q -- -q -m list :memory: "select 1"
```

For each panic, you will identify a minimal set of SQL statements that trigger the panic, and note them in a markdown
file and in an index. The markdown file (one .md file in a `github-issues/` directory) will be used to open Github
issues will contain: an issue title, the reproducer, and the error message. The index (`panic-index.txt`) will contain a
list of error messages, similar to the first code block in this document. It will also contain a list of the
statements (file:line) that were investigated, but for which the investigation was inconclusive.

# Method

1. You will first read the index. The panics in the index have already been reproduced, and don't need to be
   investigated.
2. You will identify 3-5 "panickable" statements by searching for things like, but not limited to:
    - `.unwrap()` on `Option` or `Result`
    - `.expect(...)`
    - Array/slice indexing like `[i]` without bounds checks
    - `unreachable!()` or `panic!()`
3. You will start one sub-agent per statement identified in step 2. The sub-agents' task will be to use the CLI to
   identify a set of SQL statements that trigger the panic. The sub-agents should:
    * Read the function containing the unwrap and trace backwards to understand what conditions make the statement panic
    * Identify the SQL feature that exercises this code path (e.g., specific functions, types, edge cases like NULL,
      empty strings, division by zero)
    * Start with simple SQL and incrementally add complexity
    * Try at LEAST 20 different variations of sequences of SQL statements before giving up
    * Each sub-agent should spend no more than ~10 minutes per statement before reporting.
4. Once the agents report back, you will either:
    * write a markdown issue report, and add the error message in the index. The entry will have 3 lines: first, the
      location of the corresponding markdown file, then the 2 lines identifying the error like in the code block above;
      or
    * if the agent was able to prove that the statement was safe, add a new line to the index like this one:
      `SAFE: file.rs:456 - guarded by check on line 450`; or
    * otherwise, add a new line to the index like this one:
      `INCONCLUSIVE: file.rs:789 - appears to require malformed AST`
5. Finish this iteration. Simply update the index, issues, and stop (quit).

Only perform one round (steps 1-5). The outer loop will restart you automatically.