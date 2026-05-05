import { Terminal } from "@xterm/xterm";
import { WebglAddon } from "@xterm/addon-webgl";
import { FitAddon } from "@xterm/addon-fit";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import "@xterm/xterm/css/xterm.css";

const terminal = new Terminal({
  cursorBlink: true,
  fontSize: 14,
  fontFamily: "JetBrains Mono, Fira Code, monospace",
  theme: {
    background: "#1a1a2e",
    foreground: "#e0e0e0",
  },
});

const fitAddon = new FitAddon();
terminal.loadAddon(fitAddon);

const container = document.getElementById("terminal")!;
terminal.open(container);

try {
  const webglAddon = new WebglAddon();
  terminal.loadAddon(webglAddon);
} catch {
  console.warn("WebGL addon failed to load, using canvas renderer");
}

fitAddon.fit();

function toBase64(str: string): string {
  const encoder = new TextEncoder();
  const bytes = encoder.encode(str);
  let binary = "";
  for (let i = 0; i < bytes.length; i++) {
    binary += String.fromCharCode(bytes[i]!);
  }
  return btoa(binary);
}

function fromBase64(b64: string): Uint8Array {
  const binary = atob(b64);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) {
    bytes[i] = binary.charCodeAt(i);
  }
  return bytes;
}

terminal.onData((data) => {
  const encoded = toBase64(data);
  invoke("pty_input", { data: encoded });
});

listen<string>("pty-output", (event) => {
  const bytes = fromBase64(event.payload);
  terminal.write(bytes);
});

function sendResize() {
  fitAddon.fit();
  const { cols, rows } = terminal;
  invoke("pty_resize", { cols, rows });
}

window.addEventListener("resize", sendResize);

new ResizeObserver(sendResize).observe(container);
