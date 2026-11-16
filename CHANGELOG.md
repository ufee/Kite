# Changelog

## v26.11.1

Fixes after the first install.

- The first server opens in the right pane.
- Up from a drive root on This PC shows the list of drives.
- Click the name of the selected file again to rename it; the caret goes to the end of the name.
- Line wrap in the viewer (**Alt Z**).
- **View** setting: the built-in viewer or an external app; a server file opens as a read-only copy.
- **Send** is the first item of the file menu: copy to the other pane.
- **Copy to…** opens the folder picker like **Move to…**.
- The setting is now called **Upload edits to the server on save**.
- The bird in the logo and the shortcut icon is white.

## v26.11

First release, Windows x64.

- Two file panes (this PC or a server over SFTP) and a terminal in one window.
- Server list with groups, search, several users per server, command palette.
- Login by OpenSSH agent, key file or password; host keys shared with OpenSSH `known_hosts`; **Add my SSH key** in one step.
- Transfer queue with pause, resume, retry, reconnect and restore after restart.
- Direct copy between servers with rsync and a filtered agent forward.
- Viewer and editing in an external editor with upload back and conflict check.
- Local (PowerShell, Command Prompt) and server terminals.
- Import and export of the server list as `servers.json`.
- English and Russian, light and dark themes.
- Installer and portable archive.

[Documentation](https://github.com/ufee/Kite/blob/main/docs/README.md)
