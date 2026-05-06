Du arbeitest auf dem Branch `worktree-feat+phase-1.5-gpui` im Repo `/home/developer/CascadeProjects/rift`.

AGENTS.md referenziert noch die alte Tauri-Architektur. ARCHITECTURE.md wurde bereits korrekt aktualisiert — verwende es als Referenz fuer den aktuellen Stand.

Konkrete Stellen in AGENTS.md die falsch sind:

1. "Wraps tmux with a native GUI (Tauri)" -> GPUI, kein Tauri
2. Tech stack: "TypeScript (Tauri webview)" -> kein TypeScript mehr
3. "GUI framework: Tauri v2" -> GPUI 0.2.2
4. "Build target (app): x86_64-pc-windows-msvc" -> Linux/X11 (GPUI), Windows/macOS deferred
5. Repository layout: "app/ — Tauri frontend (src-tauri/ for Rust backend, src/ for TypeScript)" -> "crates/app/ — GPUI application binary"
6. Commands: "cd app && cargo tauri dev" -> "cargo run -p rift-app"
7. "What to avoid" Abschnitt referenziert keine Tauri-spezifischen Dinge, sollte aber passen

Lies zuerst `git show worktree-feat+phase-1.5-gpui:ARCHITECTURE.md` um den aktuellen Stand zu verstehen, dann `git show worktree-feat+phase-1.5-gpui:AGENTS.md` um alle veralteten Stellen zu finden. Aktualisiere AGENTS.md so, dass es die GPUI-Architektur korrekt beschreibt. Aendere NUR was faktisch falsch ist — keine stilistischen Aenderungen, keine neuen Abschnitte.

Nicht committen. Nur die Datei editieren.
