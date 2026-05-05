use std::path::PathBuf;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::mpsc;

use rift_ssh::SshConnection;

enum PtyCommand {
    Input(Vec<u8>),
    Resize(u16, u16),
}

struct AppState {
    cmd_tx: mpsc::UnboundedSender<PtyCommand>,
}

#[tauri::command]
async fn pty_input(state: tauri::State<'_, AppState>, data: String) -> Result<(), String> {
    let bytes = BASE64.decode(&data).map_err(|e| e.to_string())?;
    state
        .cmd_tx
        .send(PtyCommand::Input(bytes))
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn pty_resize(
    state: tauri::State<'_, AppState>,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    state
        .cmd_tx
        .send(PtyCommand::Resize(cols, rows))
        .map_err(|e| e.to_string())
}

fn read_config() -> (String, u16, String, PathBuf) {
    let host = std::env::var("RIFT_SSH_HOST").unwrap_or_else(|_| "localhost".into());
    let port = std::env::var("RIFT_SSH_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(22);
    let user = std::env::var("RIFT_SSH_USER").unwrap_or_else(|_| whoami::username());
    let key_path = std::env::var("RIFT_SSH_KEY")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = dirs::home_dir().expect("no home directory");
            let ed25519 = home.join(".ssh/id_ed25519");
            if ed25519.exists() {
                ed25519
            } else {
                home.join(".ssh/id_rsa")
            }
        });
    (host, port, user, key_path)
}

pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let handle = app.handle().clone();
            let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();

            app.manage(AppState { cmd_tx });

            tauri::async_runtime::spawn(async move {
                if let Err(e) = start_ssh_session(handle, cmd_rx).await {
                    eprintln!("SSH session failed: {e}");
                }
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![pty_input, pty_resize])
        .run(tauri::generate_context!())
        .expect("failed to run rift");
}

async fn start_ssh_session(
    handle: AppHandle,
    mut cmd_rx: mpsc::UnboundedReceiver<PtyCommand>,
) -> anyhow::Result<()> {
    let (host, port, user, key_path) = read_config();

    eprintln!(
        "connecting to {user}@{host}:{port} with key {}",
        key_path.display()
    );

    let mut conn = SshConnection::connect(&host, port, &user, &key_path).await?;
    let mut pty = conn.open_pty(120, 40).await?;

    pty.write(b"tmux new-session -A -s rift\n").await?;

    loop {
        tokio::select! {
            result = pty.read() => {
                match result {
                    Ok(data) => {
                        let encoded = BASE64.encode(&data);
                        let _ = handle.emit("pty-output", encoded);
                    }
                    Err(e) => {
                        eprintln!("PTY read error: {e}");
                        break;
                    }
                }
            }
            cmd = cmd_rx.recv() => {
                match cmd {
                    Some(PtyCommand::Input(data)) => {
                        if let Err(e) = pty.write(&data).await {
                            eprintln!("PTY write error: {e}");
                            break;
                        }
                    }
                    Some(PtyCommand::Resize(cols, rows)) => {
                        if let Err(e) = pty.resize(cols, rows).await {
                            eprintln!("PTY resize error: {e}");
                        }
                    }
                    None => break,
                }
            }
        }
    }

    Ok(())
}
