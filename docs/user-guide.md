# User guide

**English** · [Русский](user-guide.ru.md)

Kite is one window with three parts: the server list on the left, two file panes, and a terminal below them. **Ctrl 1 / 2 / 3** switches between files only, both, and terminal only.

## Servers

**Add server** in the sidebar opens the server card:

| Field | Meaning |
|---|---|
| Name | Short unique name: Latin letters, digits, hyphen. Shown everywhere instead of the address (`code` in [servers.json](servers-json.md)) |
| Group | Group in the sidebar |
| Description | What runs on the server, also searchable |
| Address, Port | IP or domain, port 22 by default |
| Users | One or more logins; one is the default. Each user can have its own key |
| Default key | **Auto**: the OpenSSH agent, then `~/.ssh/id_ed25519`. **Key file**: a chosen file |
| Advanced | Start folder (home by default), keepalive interval |

**Test** checks the address and key login before saving; a password is checked on save.

### Logging in

- **Key** (recommended). Kite uses the OpenSSH agent and the keys in `~/.ssh`. An encrypted key asks for its passphrase once; tick **Add to the OpenSSH agent** and ssh, git and Kite stop asking. Kite never stores passphrases.
- **Password.** For the default user, saved in Windows Credential Manager, never in Kite files.
- **Add my SSH key.** Enter the password once: Kite logs in, appends your public key to `authorized_keys`, checks a key login and forgets the password.

Two-factor logins (more than one prompt) are not supported.

### Host keys

On the first connection Kite shows the server fingerprint and the command to verify it. Trusted keys go to `~/.ssh/known_hosts`, the same file OpenSSH uses. If a key changes, Kite stops and shows both fingerprints: after a reinstall that is expected, otherwise the connection may be intercepted.

### Groups and search

Servers are sorted by name or by recent use, grouped, and filtered by the search field (code, description, address). Right-click a server to connect as another user, edit, copy the address, disconnect or remove it from the list. Removing touches nothing on the server.

Many servers at once? Import them from a file, see [servers.json](servers-json.md).

## File panes

Each pane shows **This PC** or a server. Click a server to open it in the focused pane, **Ctrl + click** to open it in the other one. **Tab** moves focus between panes.

- **Ctrl L** edits the path: `D:\folder` locally, `/path` or `~` on a server.
- The pane menu sets visible columns (size, modified, rights, owner, group) and hidden files (**Ctrl H**).
- Right-click a file: open, view, edit, copy or move to…, rename, permissions, terminal here, delete.
- **Permissions…** sets mode bits or an octal code, optionally recursive. Other users' files need root.
- Local deletes go to the Recycle Bin; server deletes are permanent and ask first.

## Copy and move

- **F5** copies the selection to the other pane, **F6** moves it.
- Drag and drop between panes copies; **Shift + drag** moves.
- Files can be dropped from Explorer onto a pane.

Every copy goes to the **transfer queue** (status bar → **Queue**): progress, pause, resume, cancel, retry. 3 items run at once by default (setting **At once**, 1–5). A lost connection is retried after 1, 3, 10 and 30 s; after a restart unfinished items come back paused.

When a name is taken Kite asks: replace, keep both or skip, with **Remember my choice**. The default is in settings.

Kite skips what Windows cannot hold (forbidden names, names that differ only in case) and pipes, sockets, devices and links, and says so in the queue.

### Direct copy between servers

With **Copy directly between servers** on, a server-to-server copy runs `rsync` on the source server, which logs in to the target with your forwarded agent: the data never passes your PC. It needs:

- your key loaded in the OpenSSH agent and accepted on the target;
- `rsync` on both servers (a single file can fall back to `scp`);
- the target's host key already trusted in Kite;
- the source able to reach the target.

The forwarded agent only lists keys and signs, and only while the copy runs. During that time root on the source server can log in anywhere with your keys, so turn it on only for servers you trust. If a direct copy cannot start, Kite offers to copy through itself instead.

## Viewing and editing

- **F3** views a text file up to 10 MB right in the app.
- **F4** opens the file in your editor (system default, Notepad++ or VS Code, see settings). A server file is downloaded to a temporary copy; each save uploads it back through the queue.
- If the server file changed while you were editing, Kite asks whose version wins.
- With **Upload edits right away** off, each upload asks first.
- On exit Kite warns about edits not yet uploaded.

## Terminal

The terminal panel holds tabs: a local shell (PowerShell 7, Windows PowerShell or Command Prompt) or a server shell. **Ctrl `** shows or hides it.

- **Terminal here** in a pane opens a tab in that folder.
- A closed connection shows the reason; **Enter** reconnects.
- Selecting text copies it (setting **Copy on select**).
- **Save terminal history** keeps the last 1000 lines of each tab on this PC.

## Command palette

**Ctrl K** finds servers, folders and commands. Type:

- part of a code, description or address to find a server;
- `user@code` to connect as a specific user;
- `>` to show commands only (view, theme, swap panes, hidden files, new server…).

**Enter** opens in the focused pane, **Alt Enter** in the other one.

## Status bar

- **Queue** (while there are transfers) and **Errors**: the transfer queue.
- **Debug**: the operations log, what Kite did and why it failed. The debug log never contains passwords or file contents; still remove host and user names before sharing it.
- The version: click to copy it for a bug report.

The window layout, open folders and terminals are restored at the next start.
