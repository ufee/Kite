# Settings and data

**English** · [Русский](settings.ru.md)

## Settings

Open with **Ctrl ,** or the gear in the title bar.

| Section | Setting | Default |
|---|---|---|
| View | Language | System language (English or Russian) |
| | Theme: dark, light, system | Dark |
| | Window: files, both, terminal | Both |
| | Terminal font (12–16 px) | 13 |
| Connections | Check host keys | On. Off means a spoofed server goes unnoticed |
| | Copy directly between servers | Off. See [direct copy](user-guide.md#direct-copy-between-servers) |
| Servers | Import and export | |
| Transfers | At once (1–5) | 3 |
| | If the name is taken: ask, replace, keep both, skip | Ask |
| | Keep file dates when copying | On |
| Editor | App: system default, Notepad++, VS Code | System default |
| | Upload edits right away | On |
| Terminal | Local shell: PowerShell 7, Windows PowerShell, Command Prompt | PowerShell 7 |
| | Copy on select | On |
| | Save terminal history | Off |

The Connections section also shows whether the OpenSSH agent is running and how many keys it holds.

## Where data is kept

| What | Where |
|---|---|
| Servers, settings, session, transfer queue, terminal history | `%APPDATA%\io.github.ufee.kite\` |
| Temporary copies of files being edited, WebView cache | `%LOCALAPPDATA%\io.github.ufee.kite\` |
| Passwords | Windows Credential Manager, one entry per server (its default user) |
| Host keys | `~/.ssh/known_hosts`, shared with OpenSSH |
| Keys | `~/.ssh` and the OpenSSH agent; Kite only reads them |

Files in the data folder:

- `servers.json`: the server list, without passwords
- `settings.json`: settings and window layout
- `session.json`: open panes, folders and terminals
- `transfers.json`: the transfer queue
- `terms/`: terminal history, only with **Save terminal history** on

Every write is atomic. A file that no longer parses is renamed aside (`settings.json` → `settings.bad`) and Kite starts without it.

Key passphrases are never saved. The debug log is kept in memory only and is not written to disk.

## Portable mode

The portable archive has `Kite\Kite.exe` and `Kite\data\.kite-portable`. While that marker file exists, everything Kite writes (data, edit copies, WebView cache, window position) stays in `Kite\data`. Passwords and host keys still go to Credential Manager and `~/.ssh/known_hosts`: they belong to the user, not to a copy of the app.

The folder must be writable, so not under Program Files or on a read-only drive; otherwise Kite says so and exits. Delete `data\.kite-portable` and the copy behaves like an installed one.

## Moving to another PC

1. **Import and export → Export** saves `servers.json` (groups, servers, users, key names; no passwords or keys). Start folder, keepalive and the default key mode are not exported: import sets home, 30 s and Auto.
2. Copy your `~/.ssh` keys separately.
3. On the new PC: **Import**, then confirm each new host key on the first connection, and enter passwords again where needed.
