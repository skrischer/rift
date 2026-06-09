use rift_protocol::{ClientMessage, DaemonMessage, PROTOCOL_VERSION};
use tokio::sync::{broadcast, mpsc, watch};

/// Single source of truth for the daemon's observable state.
///
/// Mirrors `DaemonMessage::StateUpdate`. Held as the value of a
/// `tokio::sync::watch` channel so consumers observe the latest snapshot
/// without sharing a mutex.
#[derive(Debug, Clone, Default)]
pub struct State {
    pub sessions: Vec<String>,
}

/// Wiring for the daemon's flat dispatch loop.
///
/// Inbound `ClientMessage`s arrive on `inbound`; outbound `DaemonMessage`
/// events are published on `events`; the latest `State` snapshot is observable
/// via the `watch` receiver returned alongside this struct by [`channels`].
pub struct Daemon {
    inbound: mpsc::Receiver<ClientMessage>,
    events: broadcast::Sender<DaemonMessage>,
    state: watch::Sender<State>,
}

/// Sender handles for driving a [`Daemon`].
pub struct Handles {
    /// Send `ClientMessage`s into the dispatch loop.
    pub inbound: mpsc::Sender<ClientMessage>,
    /// Subscribe to outbound `DaemonMessage` events.
    pub events: broadcast::Sender<DaemonMessage>,
    /// Observe the latest `State` snapshot.
    pub state: watch::Receiver<State>,
}

/// Construct a [`Daemon`] and its [`Handles`] over freshly created channels.
///
/// `event_capacity` bounds the broadcast backlog; `inbound_capacity` bounds the
/// inbound mpsc queue.
pub fn channels(event_capacity: usize, inbound_capacity: usize) -> (Daemon, Handles) {
    let (inbound_tx, inbound_rx) = mpsc::channel(inbound_capacity);
    let (events_tx, _events_rx) = broadcast::channel(event_capacity);
    let (state_tx, state_rx) = watch::channel(State::default());

    let daemon = Daemon {
        inbound: inbound_rx,
        events: events_tx.clone(),
        state: state_tx,
    };
    let handles = Handles {
        inbound: inbound_tx,
        events: events_tx,
        state: state_rx,
    };
    (daemon, handles)
}

impl Daemon {
    /// Run the flat dispatch loop until the inbound channel closes.
    ///
    /// Each `ClientMessage` is matched directly to a handler. The loop owns the
    /// `State` writer and the event broadcaster; it ends when every inbound
    /// sender is dropped.
    pub async fn run(mut self) {
        while let Some(msg) = self.inbound.recv().await {
            self.dispatch(msg);
        }
    }

    /// Route a single `ClientMessage` to its handler.
    fn dispatch(&self, msg: ClientMessage) {
        match msg {
            ClientMessage::Hello { version: _ } => {
                // A receiverless broadcast send is not an error here: events are
                // fire-and-forget, so a `Welcome` with no subscriber is dropped.
                let _ = self.events.send(DaemonMessage::Welcome {
                    version: PROTOCOL_VERSION,
                });
            }
            // Terminal/tmux handling is owned by a later Phase 3 sub-spec; this
            // scaffolding only carries the handshake.
            ClientMessage::Input { .. }
            | ClientMessage::ResizePane { .. }
            | ClientMessage::TmuxCommand { .. } => {}
        }
    }

    /// Borrow the live `State` for inspection (the `watch` sender's view).
    pub fn state(&self) -> State {
        self.state.borrow().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_dispatch_hello_current_version_emits_welcome() {
        let (daemon, handles) = channels(8, 8);
        let mut events = handles.events.subscribe();
        let loop_handle = tokio::spawn(daemon.run());

        handles
            .inbound
            .send(ClientMessage::Hello {
                version: PROTOCOL_VERSION,
            })
            .await
            .expect("send Hello");

        let reply = events.recv().await.expect("receive Welcome");
        assert_eq!(
            reply,
            DaemonMessage::Welcome {
                version: PROTOCOL_VERSION,
            }
        );

        drop(handles);
        loop_handle.await.expect("dispatch loop joins cleanly");
    }
}
