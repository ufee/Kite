# servers.json

**English** · [Русский](servers-json.ru.md)

The file for moving a server list between machines or building it from notes. **Settings → Import and export** (or `>` in the palette).

## Format

```json
{
  "version": 1,
  "groups": [
    { "name": "Production", "servers": [
      { "code": "web1", "description": "Main web server", "host": "203.0.113.17", "port": 22,
        "users": [
          { "name": "root", "default": true },
          { "name": "deploy", "key": "deploy_ed25519" }
        ] }
    ] }
  ]
}
```

A flat list works too: `{ "servers": [ ... ] }`, where each server may set its own `"group"`.

| Field | Required | Rule |
|---|---|---|
| `version` | no | `1` if present |
| `code` | yes | Unique; Latin letters, digits, hyphen; starts with a letter or digit |
| `host` | yes | IP or domain, no spaces |
| `port` | no | 1–65535, default 22 |
| `description` | no | What runs on the server |
| `users` | no | Default: one user `root` |
| `users[].name` | yes | Unique within the server |
| `users[].key` | no | A file name from `~/.ssh`, no path. Omitted or `auto`: agent, then `id_ed25519` |
| `users[].default` | no | The default login; otherwise the first user |

Passwords, passphrases and private keys never go into this file. Maximum size 2 MB.

## Import

Drop the file, choose it, or paste with **From clipboard**; **Example** loads a sample. Kite matches servers by `code` and shows the changes before applying them: new, changed (address, port, description, group, users), unchanged. **Delete servers missing from the file** also removes the servers the file does not have; it is off by default.

## Export

Choose the groups and whether to include **Users and key names**, then **Save** to a file or **Copy** the JSON. The result has the same format and imports back as is.

## Building it with an AI agent

**Copy for an agent** puts a ready prompt with this format on the clipboard. Paste it into any assistant together with your notes about the servers, then import the answer.
