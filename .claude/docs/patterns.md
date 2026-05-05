# Coding patterns

Reference document for implementation patterns used in this project. Read this before implementing new features — don't rely on general Rust knowledge for project-specific conventions.

## Error handling

Use `thiserror` for typed errors in library crates, `anyhow` in binaries for ergonomic propagation.

```rust
// Library crate (e.g. tmux-core)
#[derive(Debug, thiserror::Error)]
pub enum TmuxError {
    #[error("failed to parse control mode event: {0}")]
    ParseError(String),
    #[error("session not found: {0}")]
    SessionNotFound(String),
}

// Binary crate (daemon)
fn main() -> anyhow::Result<()> {
    // ...
}
```

Never `.unwrap()` in library code. `.expect("reason")` only for invariants that cannot be violated at runtime.

## State management

The daemon's `State` struct is the single source of truth for tmux sessions, pane contents, and filesystem state. All mutations flow through a central state manager.

Notify consumers via channels, not shared mutexes:

```rust
// Prefer this
let (tx, rx) = tokio::sync::watch::channel(initial_state);

// Over this
let state = Arc::new(Mutex::new(initial_state)); // avoid
```

Use `watch` for "latest value" semantics (UI state), `broadcast` for event streams (file events, pane output).

## Parsing

tmux control mode and VTE streams are parsed with explicit state machines or `nom` combinators. No regex for structured protocol parsing. Regex is acceptable for one-off text extraction in non-critical paths.

## Async discipline

All I/O is async via Tokio. CPU-bound work on `spawn_blocking`:

```rust
// VTE parsing is CPU-bound
let grid = tokio::task::spawn_blocking(move || {
    parser.process_bytes(&raw_output)
}).await?;
```

Never call blocking functions (std::fs, std::net) inside async contexts.
