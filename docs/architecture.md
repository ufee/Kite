# Architecture

**English** · [Русский](architecture.ru.md)

Two parts in one process: a Rust core (`src-tauri/`) that does all network and disk work, and a Vue 3 interface (`ui/`) in the system WebView2. They talk over Tauri IPC: the UI calls commands, the core sends events.

```
ui/ (Vue 3, TypeScript)          src-tauri/ (Rust, Tauri 2)
  components/  state/              ipc/  ── thin commands, errors as codes
  backend/ ── invoke / events ──►  ssh/  fs/  term/  transfer/  edit/
                                   store/  secrets/  log/
```

## Core: `src-tauri/src`

| Module | Role |
|---|---|
| `ipc/` | Commands the UI calls. Moves work off the async runtime, maps errors |
| `ssh/` | Connections ([russh](https://crates.io/crates/russh)): one per server and user, carries SFTP and terminals. Host keys against `~/.ssh/known_hosts`, login order, passphrase and host key prompts, filtered agent forwarding |
| `fs/` | Pane file operations: local disk and SFTP. Real OS errors go to the UI, no guessing from permission bits |
| `term/` | Local shell on ConPTY and shells on SSH channels; one ordered input queue per tab |
| `transfer/` | The transfer queue: FIFO, 1–5 at a time, own connections per server and user, retries, direct server-to-server copy with rsync |
| `edit/` | External editor: temp copy, folder watch, upload through the queue, conflict check |
| `store/` | JSON files in the data folder, atomic writes; `servers.json` import parsing |
| `secrets/` | Passwords in the OS keychain |
| `log/` | Debug log: an in-memory ring of 1000 rows sent to the UI as events |
| `portable.rs` | Portable mode switch |
| `webview.rs` | WebView hardening: no browser keys, menus, zoom or navigation |
| `error.rs` | Errors that cross into the UI: a code and facts, never wording |

Rules the core keeps:

- **Secrets never reach the log.** Passwords and passphrases live in `Zeroizing` buffers passed only to the SSH layer; file contents are logged as sizes.
- **No burst of connections.** At most three SSH handshakes at once.
- **Reconnect** after 1, 3, 10 and 30 s, then report.
- **The core owns long state.** The queue survives a UI reload and is saved to `transfers.json`.
- **The UI has no file system or dialog permissions.** `capabilities/default.json` allows only window controls.

## Interface: `ui/src`

| Folder | Role |
|---|---|
| `components/` | Window parts: sidebar, panes, terminal, status bar, palette, dialogs |
| `state/` | Reactive state by area: panes, servers, queue, terminals, settings, layout, session persistence |
| `backend/` | The `Backend` interface with two implementations: `tauri.ts` (the core) and `mock/` (in-memory servers and files) |
| `i18n/` | Russian strings are the message ids in code; `en.ts` maps them to English |
| `platform/` | Window controls, WebView key guard |

`npm run ui` runs the interface in a plain browser on the mock backend: everything except window controls works without the core or a server.

## Data flow examples

- **Open a server folder:** pane → `fs_list` → `ssh` gets or opens the session → SFTP `readdir` → rows back.
- **Copy:** `transfer_enqueue` → queue item → `transfer://state` events (at most 10 per second) → `transfer://batch` when the batch ends.
- **Edit a server file:** `edit_open` downloads a copy and starts the editor → save detected → size and date compared with the server → upload as a queue item.
