# Contributing

Thanks for looking. Kite is a small personal project, so a short note first saves both of us time.

- **Bugs:** open an issue with the version (click it in the status bar to copy), what you did and what you saw. The **Debug** log in the status bar often has the reason; remove host names and user names before pasting it.
- **Changes:** open an issue before a larger pull request, so we agree on the approach. Small fixes can go straight to a pull request.
- **Security issues:** never in a public issue, see [SECURITY.md](SECURITY.md).

## Before a pull request

```bash
npm ci
npm run check                                   # UI types
cd src-tauri
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
```

CI runs the same checks and a secret scan. Comments in code are in English. Interface strings are written in Russian in the code; the English text goes to `ui/src/i18n/en.ts`.

By sending a pull request you agree that your change is released under the [MIT License](LICENSE).
