# PgKronika agent instructions

## Standing Review Rule: Memory Bounds

Every diff review, manual or automated, must include a memory-bounds check.
The collector runs on the database host; an out-of-memory failure there is
worse than a lost segment.

For each new or changed code path the review must answer:

1. **What is the peak memory, and what enforces the bound?** Acceptable
   bounds are a config limit (`DictLimits`, `JournalLimits`,
   `JournalConfig::max_journal_len`), a format constant, or the size of an
   input the caller already holds. "Usually small" is not a bound.
2. **Is anything sized by the outside world materialized whole?** Reading an
   entire file, journal, or section into memory when its size is controlled
   by external input is forbidden — stream with a fixed-size buffer instead
   (see `Journal::open` recovery as the reference pattern).
3. **Does any structure grow without an enforced limit?** Unbounded growth
   is allowed only with an enforced cap and a typed overflow signal that
   tells the caller what to do (reference: `JournalError::Full` → merge
   early and reset).
4. **Are doc claims about memory accurate?** If a doc says "one part in
   memory", the review must check that this is literally true, including
   per-item directories, clones, and growth slack.

Independent review workflows must include a dedicated memory-bounds pass
alongside bugs, spec, and tests. Verify findings with allocation counting or
pathological inputs where practical.

## Standing Review Rule: Comment Quality

Every diff review and pre-commit pass must include a comment-quality check,
with the same standing as the memory-bounds check. Apply the local
`code-comments` rules whenever writing or reviewing comments. A comment that
only restates the code is a defect to delete, not decoration to keep.

For every comment and doc-comment in the diff the review must answer:

1. **Does it say something the code does not?** A comment that paraphrases the
   next line is the *what* — delete it. A comment earns its place only by
   carrying what the code cannot: rationale, an invariant the types don't
   enforce, a trade-off, a footgun, or a pointer to the contract.
2. **Will it survive a plausible refactor?** Line-by-line narration goes stale
   when the code moves. Rewrite to durable intent, or — better — extract a
   named function so the name carries the meaning the compiler can check.
3. **Is it terse and at the right density?** No `// Note that`, no preamble, no
   restating the function name. Dense mechanical code gets no per-line
   commentary; the one subtle line gets one.
4. **Do doc-comments state the contract, not the body?** Public `///` items
   give `# Errors`/`# Panics`/bounds and what the caller can rely on — not a
   narration of the implementation, which is free to change.

Independent review workflows must include a dedicated comment-quality pass
alongside bugs, spec, tests, and memory bounds.

## Other standing gates

- `cargo fmt --all --check`, strict clippy (workspace lints, warnings are
  errors in CI), `cargo test --workspace`, `cargo run -p xtask -- check-deps`.
- Code comments and rustdoc reference crate README sections, never `docs/`
  paths (the design archive in `docs/` will be deleted).
- Crate READMEs are the contract documentation: English `README.md` with a
  Russian `README.ru.md` mirror, kept in sync.
