Du arbeitest auf dem Branch `worktree-feat+phase-1.5-gpui` im Repo `/home/developer/CascadeProjects/rift`.

## Problem

In `crates/app/src/main.rs` Zeilen 127-128 (ungefaehr) werden bei Session-Ende die Tokio-Tasks mit `abort()` beendet. Der `pty_tx` Sender wird gedroppt, `pty_rx.try_recv()` in `process_incoming()` (view.rs) gibt `TryRecvError::Disconnected` zurueck — das wird ignoriert. Terminal friert still ein.

## Referenz: termy

termy (in `/home/developer/CascadeProjects/termy`) nutzt graceful shutdown via Message-Passing:

1. **Shutdown-Signal**: `Terminal::drop()` sendet `EventLoopMsg::Shutdown` ueber Channel (`crates/terminal_ui/src/runtime.rs:1424-1428`). Kein `abort()`.
2. **Event-Loop Exit**: Loop bricht sauber ab, PTY wird deregistriert, Child-Process gereapt (`runtime.rs:821-826`).
3. **UI-Reaktion**: `TerminalEvent::Exit` wird verarbeitet (`src/terminal_view/mod.rs:4643-4806`). App quit nur wenn letzter Tab — sonst bleibt Content sichtbar.

## Fix

Rift hat eine andere Architektur (SSH/Tokio statt lokaler PTY), aber das Prinzip ist gleich:

### 1. `crates/app/src/main.rs` — Graceful Shutdown

Ersetze `abort()` durch sauberen Shutdown:
- Droppe `pty_tx` explizit (signalisiert dem GPUI-Side dass die Session vorbei ist)
- Warte auf die Task-Handles: `let _ = write_handle.await;` und `let _ = resize_handle.await;`
- Oder: Sende ein explizites Shutdown-Signal ueber einen separaten `oneshot` Channel

### 2. `crates/terminal/src/view.rs` — Disconnect erkennen

Wenn `pty_rx` disconnected ist (Sender gedroppt):
- Setze ein `session_ended: bool` Flag
- Rufe `cx.quit()` auf (einfachste Loesung fuer Single-Terminal-App)
- Oder: Zeige "[Session ended]" Text im Terminal (wie termy bei letztem Tab)

Die Erkennung haengt davon ab ob der M1-Fix (event-driven rendering) schon implementiert ist:
- **Mit M1-Fix**: Der async PTY-Task bekommt `Disconnected` vom `recv_async().await` -> kann direkt `cx.quit()` aufrufen
- **Ohne M1-Fix**: `process_incoming()` muss `TryRecvError::Disconnected` separat von `Empty` behandeln

Lies beide Dateien, verstehe den aktuellen Flow, und implementiere den Fix. Nicht committen.
