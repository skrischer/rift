use std::env;
use std::path::PathBuf;
use std::thread;

use anyhow::{Context as _, Result};
use gpui::*;
use gpui_component::{Root, Theme, ThemeMode, ThemeRegistry};
use rift_terminal::{PaneInput, PaneOutput, SessionView, SubscriptionUpdate, TermSize};
use tracing::{debug, error, info, warn};
use tracing_subscriber::EnvFilter;

/// Catppuccin Mocha theme in gpui-component's native theme format. Registered in
/// the `ThemeRegistry` alongside the built-in Light/Dark themes, leaving room to
/// add more selectable themes later.
const CATPPUCCIN_MOCHA: &str = include_str!("../assets/themes/catppuccin-mocha.json");

/// Register the Catppuccin theme and make it the active app-wide theme so all
/// gpui-component widgets render in rift's palette instead of the default light theme.
fn apply_theme(cx: &mut App) {
    if let Err(e) = ThemeRegistry::global_mut(cx).load_themes_from_str(CATPPUCCIN_MOCHA) {
        error!(%e, "failed to load catppuccin theme");
        return;
    }
    let Some(theme) = ThemeRegistry::global(cx)
        .themes()
        .get(&SharedString::from("Catppuccin Mocha"))
        .cloned()
    else {
        error!("catppuccin theme not found after load");
        return;
    };
    Theme::global_mut(cx).dark_theme = theme;
    Theme::change(ThemeMode::Dark, None, cx);
}

struct SshConfig {
    host: String,
    port: u16,
    user: String,
    key: PathBuf,
}

struct PtyChannels {
    pane_output_tx: flume::Sender<PaneOutput>,
    input_rx: flume::Receiver<PaneInput>,
    size_changed_rx: flume::Receiver<TermSize>,
    snapshot_tx: flume::Sender<termy_terminal_ui::TmuxSnapshot>,
    tmux_command_rx: flume::Receiver<String>,
    subscription_tx: flume::Sender<SubscriptionUpdate>,
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_target(true)
        .init();

    info!(
        os = env::consts::OS,
        arch = env::consts::ARCH,
        "rift starting"
    );

    Application::with_platform(gpui_platform::current_platform(false)).run(|cx: &mut App| {
        gpui_component::init(cx);
        apply_theme(cx);
        let bounds = Bounds::centered(None, size(px(1200.0), px(800.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Maximized(bounds)),
                ..Default::default()
            },
            |window, cx| {
                let session_view = cx.new(|cx| {
                    let (view, handle) = SessionView::new(cx);

                    let ssh = SshConfig {
                        host: env::var("RIFT_SSH_HOST").unwrap_or_else(|_| "127.0.0.1".into()),
                        user: env::var("RIFT_SSH_USER").unwrap_or_else(|_| "developer".into()),
                        port: env::var("RIFT_SSH_PORT")
                            .ok()
                            .and_then(|p| p.parse().ok())
                            .unwrap_or(22),
                        key: env::var("RIFT_SSH_KEY")
                            .map(PathBuf::from)
                            .unwrap_or_else(|_| {
                                let home = env::var("USERPROFILE")
                                    .or_else(|_| env::var("HOME"))
                                    .unwrap_or_else(|_| {
                                        if cfg!(target_os = "windows") {
                                            "C:\\Users\\Default".into()
                                        } else {
                                            "/home/developer".into()
                                        }
                                    });
                                PathBuf::from(home).join(".ssh").join("id_ed25519")
                            }),
                    };

                    let channels = PtyChannels {
                        pane_output_tx: handle.pane_output_tx,
                        input_rx: handle.input_rx,
                        size_changed_rx: handle.size_changed_rx,
                        snapshot_tx: handle.snapshot_tx,
                        tmux_command_rx: handle.tmux_command_rx,
                        subscription_tx: handle.subscription_tx,
                    };

                    let key_exists = ssh.key.exists();
                    debug!(
                        host = %ssh.host,
                        port = ssh.port,
                        user = %ssh.user,
                        key = %ssh.key.display(),
                        key_exists,
                        "connecting via SSH"
                    );

                    thread::spawn(move || {
                        let rt =
                            tokio::runtime::Runtime::new().expect("failed to create tokio runtime");
                        rt.block_on(async move {
                            if let Err(e) = run_ssh_session(&ssh, channels).await {
                                error!(
                                    %e,
                                    host = %ssh.host,
                                    port = ssh.port,
                                    key = %ssh.key.display(),
                                    key_exists,
                                    "SSH session failed"
                                );
                            }
                        });
                    });

                    view
                });

                let focus_handle = session_view.focus_handle(cx);
                window.defer(cx, move |window, cx| {
                    focus_handle.focus(window, cx);
                });

                cx.new(|cx| Root::new(session_view, window, cx))
            },
        )
        .unwrap();
        cx.activate(true);
    });
}

async fn run_ssh_session(ssh: &SshConfig, ch: PtyChannels) -> Result<()> {
    use rift_ssh::SshConnection;
    use termy_terminal_ui::{TmuxClient, TmuxNotification, TmuxSocketTarget};

    let mut conn = SshConnection::connect(&ssh.host, ssh.port, &ssh.user, &ssh.key)
        .await
        .context("SSH connection failed")?;

    let pty = conn
        .open_pty_exec(80, 24, "tmux -CC new-session -A -s rift")
        .await
        .context("failed to start tmux control mode")?;

    let reader = pty.sync_reader();
    let writer = pty.sync_writer();

    let (wakeup_tx, wakeup_rx) = flume::bounded::<()>(1);

    let tmux_client = TmuxClient::from_streams(
        writer,
        reader,
        "rift".to_string(),
        "tmux".to_string(),
        TmuxSocketTarget::Default,
        Some(wakeup_tx),
    )
    .context("failed to create tmux control client")?;

    tmux_client
        .set_client_size(80, 24)
        .context("failed to set initial tmux client size")?;

    tmux_client
        .send_command_async("refresh-client -f pause-after=5")
        .context("failed to activate flow control")?;

    // Register format subscriptions so pane/window state changes (cd, command,
    // window rename) stream in within ~1s instead of waiting for a structural
    // refresh. Requires tmux 3.4+; on older servers each call returns an error
    // and we degrade to snapshot-only rather than failing the session.
    for (name, scope, format) in [
        ("rift_pane_path", "%*", "#{pane_current_path}"),
        ("rift_pane_command", "%*", "#{pane_current_command}"),
        ("rift_window_name", "@*", "#{window_name}"),
    ] {
        if let Err(e) = tmux_client.subscribe(name, scope, format) {
            warn!(%e, name, "failed to register tmux subscription; continuing snapshot-only");
        }
    }

    info!("tmux control mode connected");

    let pane_output_tx = ch.pane_output_tx;
    let input_rx = ch.input_rx;
    let size_changed_rx = ch.size_changed_rx;
    let snapshot_tx = ch.snapshot_tx;
    let tmux_command_rx = ch.tmux_command_rx;
    let subscription_tx = ch.subscription_tx;

    let initial_snapshot = tmux_client
        .refresh_snapshot()
        .context("failed to get initial tmux snapshot")?;
    let _ = snapshot_tx.send(initial_snapshot);

    let tmux_for_input = std::sync::Arc::new(tmux_client);
    let tmux_for_resize = tmux_for_input.clone();
    let tmux_for_poll = tmux_for_input.clone();
    let tmux_for_cmd = tmux_for_input.clone();

    let input_handle = std::thread::spawn(move || {
        while let Ok(input) = input_rx.recv() {
            if tmux_for_input
                .send_input(&input.pane_id, &input.bytes)
                .is_err()
            {
                break;
            }
        }
    });

    let resize_handle = std::thread::spawn(move || {
        while let Ok(new_size) = size_changed_rx.recv() {
            if tmux_for_resize
                .set_client_size(new_size.cols as u16, new_size.rows as u16)
                .is_err()
            {
                break;
            }
        }
    });

    let cmd_handle = std::thread::spawn(move || {
        while let Ok(cmd) = tmux_command_rx.recv() {
            debug!(cmd = %cmd, "sending tmux command");
            if tmux_for_cmd.send_command_async(&cmd).is_err() {
                break;
            }
        }
    });

    let poll_handle = std::thread::spawn(move || loop {
        if wakeup_rx.recv().is_err() {
            break;
        }
        let notifications = tmux_for_poll.poll_notifications();
        let mut should_exit = false;
        for notification in notifications {
            match notification {
                TmuxNotification::Output { pane_id, bytes } => {
                    if pane_output_tx.send(PaneOutput { pane_id, bytes }).is_err() {
                        should_exit = true;
                        break;
                    }
                }
                TmuxNotification::NeedsRefresh => {
                    if let Ok(snapshot) = tmux_for_poll.refresh_snapshot() {
                        let _ = snapshot_tx.send(snapshot);
                    }
                }
                TmuxNotification::SubscriptionChanged {
                    name,
                    session,
                    window,
                    pane,
                    value,
                } => {
                    if subscription_tx
                        .send(SubscriptionUpdate {
                            name,
                            session,
                            window,
                            pane,
                            value,
                        })
                        .is_err()
                    {
                        should_exit = true;
                        break;
                    }
                }
                TmuxNotification::Exit(reason) => {
                    info!(?reason, "tmux control mode exited");
                    should_exit = true;
                    break;
                }
                TmuxNotification::Warning(msg) => {
                    tracing::warn!(%msg, "tmux control warning");
                }
            }
        }
        if should_exit {
            break;
        }
    });

    let _ = poll_handle.join();
    let _ = input_handle.join();
    let _ = resize_handle.join();
    let _ = cmd_handle.join();
    Ok(())
}
