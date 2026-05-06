Du arbeitest auf dem Branch `worktree-feat+phase-1.5-gpui` im Repo `/home/developer/CascadeProjects/rift`.

## Problem

In `crates/terminal/src/view.rs` Zeile 249 (ungefaehr) wird `window.request_animation_frame()` bedingungslos bei jedem Render aufgerufen. Das pinnt die CPU auf ~60 FPS permanent — auch bei idle Terminal. Ausserdem ist `process_incoming()` (PTY-Daten lesen) an diesen Frame-Loop gekoppelt: ohne Frame-Loop werden keine PTY-Daten verarbeitet.

## Referenz: termy

termy (in `/home/developer/CascadeProjects/termy`) nutzt ein event-driven Rendering ohne `request_animation_frame`:

1. **Cursor Blink**: `smol::Timer::after(Duration::from_millis(530))` in einem async Loop (`src/terminal_view/mod.rs:3878-3899`). `cx.notify()` wird nur aufgerufen wenn `tick_cursor_blink()` true zurueckgibt (State hat sich tatsaechlich geaendert).

2. **PTY-Daten**: Event-Listener sendet `wake_tx.try_send(())` wenn neue Daten ankommen -> GPUI wird benachrichtigt -> Render.

3. **Idle**: Kein Render, keine CPU-Last.

## Fix

Ersetze den `request_animation_frame`-Loop durch event-driven Rendering:

### 1. PTY-Daten: Async Task statt Polling

Aktuell wird `process_incoming()` im Render-Pfad aufgerufen und pollt `pty_rx.try_recv()`. Stattdessen:

- Spawne einen GPUI-async-Task (`cx.spawn()`) der auf `pty_rx.recv_async().await` wartet
- Wenn Daten ankommen: `parser.advance()` + `cx.notify()` aufrufen
- `process_incoming()` aus dem Render-Pfad entfernen

### 2. Cursor Blink: smol::Timer

- Spawne einen zweiten GPUI-async-Task mit `smol::Timer::after(Duration::from_millis(500))`
- Toggle `cursor_blink_visible` nur wenn `cursor_style.blinking` aktiv ist
- `cx.notify()` nur bei State-Change

### 3. request_animation_frame entfernen

Komplett entfernen. Alle Redraws kommen jetzt ueber `cx.notify()` aus:
- PTY-Daten async Task
- Cursor Blink Timer
- Event-Handler (on_key_down, on_mouse_down, on_scroll)
- Resize

Lies die aktuelle view.rs, verstehe wie `process_incoming` aufgerufen wird und wie die `pty_rx` Channel verwendet werden. Schaue dir termys Pattern in `src/terminal_view/mod.rs:3878-3899` und `crates/terminal_ui/src/runtime.rs:870-920` an.

Nicht committen.
