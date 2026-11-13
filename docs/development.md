# Development

**English** · [Русский](development.ru.md)

## Requirements

- Node.js 22.12+
- Stable Rust and the [Tauri prerequisites](https://tauri.app/start/prerequisites/) (for the desktop app only)
- Windows 10 or 11 with WebView2

## Commands

```bash
npm install
npm run ui      # interface only, browser at http://localhost:5174, mock backend
npm run dev     # desktop app with hot reload of the UI
npm run check   # UI type check
npm run build   # release build
```

Core checks, from `src-tauri/`:

```bash
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
```

## Conventions

- Code comments in English.
- Interface strings are written in Russian in the code and translated in `ui/src/i18n/en.ts`. A new string needs an entry there.
- Errors from the core carry a code and values; the UI words them.
- Dependencies are pinned; `--locked` everywhere.
- Changes go through a pull request; see [CONTRIBUTING.md](../CONTRIBUTING.md).

## CI

Every push to `main` and every pull request runs:

1. Secret scan.
2. UI build, third-party license generation (`scripts/third-party.mjs`), Clippy and tests on Windows.

## Release

1. Set the version in `src-tauri/tauri.conf.json`, the only place it lives.
2. Add a `## v<version>` section to `CHANGELOG.md` (a trailing `.0` is dropped: `## v26.11`). It becomes the release notes; without it the build fails.
3. Push a tag `v<version>`. A tag that does not match the version fails the build.
4. GitHub Actions builds from a clean state (no caches) and opens a draft release with:
   - `Kite-v<version>-setup.exe`: per-user NSIS installer;
   - `Kite-v<version>-portable.zip`: `Kite.exe` with `data\.kite-portable` (`scripts/package.ps1`);
   - `SHA256SUMS.txt`.
5. Check the draft and publish it.
