# rift — CLAUDE.md

@AGENTS.md

## Claude-specific

When compacting, always preserve: the list of modified files, active crate context, and any failing test output.

For detailed patterns (error handling, state management, async discipline), read `.claude/docs/patterns.md` before implementing — don't guess from memory.

For protocol message types, read `.claude/docs/protocol.md` before adding or modifying messages.

## Workflow

1. Check architecture docs before adding dependencies or new crates.
2. Run `cargo clippy --workspace -- -D warnings` and `cargo test --workspace` before considering a task done.
3. Don't refactor code you weren't asked to touch. Note opportunities in a comment if significant.
4. When in doubt about a design decision, state your reasoning — don't silently pick an approach.
5. Respect crate boundaries. Adding to `protocol` is a deliberate API change.
