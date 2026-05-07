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

This project runs **two independent async runtimes** — this is forced by dependencies, not a design choice:

| | GPUI (UI thread) | SSH (dedicated OS thread) |
|---|---|---|
| **Runtime** | smol (via GPUI internals) | Tokio |
| **Spawn** | `cx.spawn()` | `tokio::spawn()` |
| **Timer/sleep** | `smol::Timer::after()` | `tokio::time::sleep()` |
| **CPU offload** | `smol::unblock()` | `tokio::task::spawn_blocking()` |
| **Bridge** | flume channels | flume channels |

GPUI owns the main event loop (`Application::new().run()`). Tokio lives in a separate OS thread created via `std::thread::spawn` + `Runtime::new()`.

### Rules per runtime

**GPUI/smol side** (anything in `cx.spawn`, render, event handlers):
- Use `smol::Timer` for delays — `tokio::time::sleep` will not fire here
- Use `smol::unblock` to offload CPU-bound work (VTE parsing, grid snapshots)
- `tokio::task::spawn_blocking` is **not available** in this context
- Never hold `Mutex` locks across `.await` points

**Tokio side** (SSH connection, PTY I/O, daemon):
- Use `tokio::task::spawn_blocking` for blocking I/O (`std::fs`, `russh_keys::load_secret_key`)
- Use `tokio::fs` instead of `std::fs` in async functions
- Never call `std::thread::sleep` — use `tokio::time::sleep`

**Cross-runtime bridge:**
- `flume` channels connect the two runtimes (PTY data, input, resize signals)
- `Arc<Mutex<T>>` only where channel-based snapshots are not feasible (e.g. `alacritty_terminal::Term` shared between PTY loop and UI render)

```rust
// GPUI side: offload CPU-bound parsing
let result = smol::unblock(move || {
    parser.process_bytes(&raw_output)
}).await;

// Tokio side: offload blocking file I/O
let key = tokio::task::spawn_blocking(move || {
    russh_keys::load_secret_key(&path, None)
}).await??;
```

Never call blocking functions (`std::fs`, `std::net`, `Path::exists()`) inside async contexts on either runtime. Evaluate them before the first `.await` point or offload to a blocking thread.
