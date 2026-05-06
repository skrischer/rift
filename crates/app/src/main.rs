use std::env;
use std::path::PathBuf;
use std::thread;

use anyhow::{Context as _, Result};
use gpui::*;
use rift_terminal::{TermSize, TerminalView};

struct SshConfig {
    host: String,
    port: u16,
    user: String,
    key: PathBuf,
}

struct PtyChannels {
    pty_tx: flume::Sender<Vec<u8>>,
    input_rx: flume::Receiver<Vec<u8>>,
    size_changed_rx: flume::Receiver<TermSize>,
}

fn main() {
    Application::new().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(1200.0), px(800.0)), cx);
        let window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    ..Default::default()
                },
                |_window, cx| {
                    cx.new(|cx| {
                        let (view, handle) = TerminalView::new(cx);

                        let ssh = SshConfig {
                            host: env::var("RIFT_SSH_HOST").unwrap_or_else(|_| "localhost".into()),
                            user: env::var("RIFT_SSH_USER").unwrap_or_else(|_| "developer".into()),
                            port: env::var("RIFT_SSH_PORT")
                                .ok()
                                .and_then(|p| p.parse().ok())
                                .unwrap_or(22),
                            key: env::var("RIFT_SSH_KEY")
                                .map(PathBuf::from)
                                .unwrap_or_else(|_| {
                                    let home = env::var("HOME")
                                        .unwrap_or_else(|_| "/home/developer".into());
                                    PathBuf::from(home).join(".ssh/id_ed25519")
                                }),
                        };

                        let channels = PtyChannels {
                            pty_tx: handle.pty_tx,
                            input_rx: handle.input_rx,
                            size_changed_rx: handle.size_changed_rx,
                        };

                        thread::spawn(move || {
                            let rt = tokio::runtime::Runtime::new()
                                .expect("failed to create tokio runtime");
                            rt.block_on(async move {
                                if let Err(e) = run_ssh_session(&ssh, channels).await {
                                    eprintln!("SSH session error: {e:#}");
                                }
                            });
                        });

                        view
                    })
                },
            )
            .unwrap();
        window
            .update(cx, |view, window, cx| {
                window.focus(&view.focus_handle(cx));
            })
            .unwrap();
        cx.activate(true);
    });
}

async fn run_ssh_session(ssh: &SshConfig, ch: PtyChannels) -> Result<()> {
    use rift_ssh::SshConnection;

    let mut conn = SshConnection::connect(&ssh.host, ssh.port, &ssh.user, &ssh.key)
        .await
        .context("SSH connection failed")?;

    let pty = conn.open_pty(80, 24).await.context("failed to open PTY")?;

    let pty_writer = pty.clone_writer();

    pty_writer
        .write(b"tmux new-session -A -s rift\n")
        .await
        .context("failed to start tmux")?;

    let write_handle = tokio::spawn({
        let input_rx = ch.input_rx.clone();
        let pty_writer = pty_writer.clone();
        async move {
            while let Ok(data) = input_rx.recv_async().await {
                if pty_writer.write(&data).await.is_err() {
                    break;
                }
            }
        }
    });

    let resize_handle = tokio::spawn({
        let pty_writer = pty_writer.clone();
        let size_changed_rx = ch.size_changed_rx;
        async move {
            while let Ok(new_size) = size_changed_rx.recv_async().await {
                let _ = pty_writer
                    .resize(new_size.cols as u16, new_size.rows as u16)
                    .await;
            }
        }
    });

    while let Ok(data) = pty.read().await {
        if ch.pty_tx.send(data).is_err() {
            break;
        }
    }

    write_handle.abort();
    resize_handle.abort();
    Ok(())
}
