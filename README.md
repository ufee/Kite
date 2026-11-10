# Kite

**English** · [Русский](README.ru.md)

SSH and SFTP in one window: two file panes and a terminal side by side, for people who keep a lot of servers.

> **Status:** early development. The interface runs on a built-in mock backend with demo servers; the real SSH core is not wired in yet.

## Stack

- [Tauri 2](https://tauri.app) shell, Rust core (`src-tauri/`)
- Vue 3 + TypeScript + Vite interface (`ui/`), [xterm.js](https://xtermjs.org) terminal
- Windows first; other platforms later

## Development

Requirements: Node.js 22.12+ and, for the desktop shell, a stable Rust toolchain with the [Tauri prerequisites](https://tauri.app/start/prerequisites/).

```bash
npm install
npm run ui      # interface only, in a browser at http://localhost:5174
npm run dev     # desktop app
npm run check   # type check
npm run build   # release build
```

In the browser the app runs on the mock backend, so everything except window controls works without a server.

## License

The source code is released under the [MIT License](LICENSE).

The name **Kite** and the logo (files in `brand/` and the application icons in `src-tauri/icons/`) are not covered by the MIT License. You may not use them for your own products or forks without permission; please rename and rebrand a fork you distribute.
