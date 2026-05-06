Du arbeitest auf dem Branch `worktree-feat+phase-1.5-gpui` im Repo `/home/developer/CascadeProjects/rift`.

## Problem

In `crates/terminal/src/view.rs` Zeilen 139-151 (ungefaehr) gibt es eine Scroll-Preservation-Logik in `process_incoming()` die fehlerhaft ist. Sie speichert `display_offset` vor dem Parser-Advance, und versucht danach den Offset per `Scroll::Delta` wiederherzustellen. Das fuehrt zu Viewport-Jitter bei aktivem Output wenn der User gescrollt hat.

## Referenz: termy

termy (in `/home/developer/CascadeProjects/termy`) hat KEINE Scroll-Preservation-Logik.

- `pane_terminal.rs:55-64`: `feed_output()` ruft nur `parser.advance(&mut *term, bytes)` auf — kein offset-before/after, kein Delta.
- alacritty_terminal behaelt `display_offset` automatisch bei wenn > 0. Neuer Output veraendert den Offset nicht.
- Snap-to-Bottom passiert nur explizit bei Keyboard-Input via `scroll_to_bottom()` (`interaction/input.rs:448-453`).
- Bonus: termy hat 160ms Scroll-Suppression nach Input gegen Momentum-Scroll (`INPUT_SCROLL_SUPPRESS_MS`, `mod.rs:141`).

## Fix

1. Entferne die gesamte Scroll-Preservation-Logik aus `process_incoming()`. Kein `offset_before`, kein `Scroll::Delta`. Nur `parser.advance()` aufrufen.
2. Verifiziere dass Snap-to-Bottom bei Keyboard-Input korrekt funktioniert (sollte bereits im `on_key_down` Handler sein via `term.scroll_display(Scroll::Bottom)`).
3. Optional: Erwaege 160ms Scroll-Suppression nach Input (termy-Pattern) als Follow-up.

Lies die Datei, finde die exakte Stelle, und entferne den ueberfluessigen Code. Nicht committen.
