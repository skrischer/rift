//! rift's own tmux control-mode client.
//!
//! A byte-oriented parser for the control-mode notification stream and the
//! `%begin`/`%end`/`%error` command-response guards, plus command emission with
//! FIFO response correlation and connection state. The crate is pure (no I/O,
//! no async): the daemon owns the `tmux -C` child process and its pipes and
//! drives this client by feeding the bytes tmux writes ([`Client::feed`]) and
//! writing back the bytes of the commands it emits ([`Client::send_command`]).
//!
//! Written against the tmux man page, wiki, `control.c`, and
//! `docs/tmux-reference.md` (control mode has no formal spec); tested against
//! real-tmux fixtures, valid and malformed. Deliberately `gpui`-free and
//! musl-clean so it links into the daemon's static build (see
//! `docs/spec-terminal-streaming.md`).

mod client;
mod error;
mod event;
mod parser;
mod vis;

pub use client::{Client, Command, CommandId, ConnectionState};
pub use error::TmuxError;
pub use event::Event;

pub type Result<T> = std::result::Result<T, TmuxError>;
