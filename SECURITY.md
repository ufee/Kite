# Security

Kite holds SSH access to servers, so a vulnerability here can matter a lot. Please report it privately.

## Reporting

Use [Report a vulnerability](https://github.com/ufee/Kite/security/advisories/new) on GitHub (Security → Advisories). Only the maintainer sees the report. Do not open a public issue.

Include the version (click it in the status bar to copy), what an attacker can do and the steps to reproduce. You get an answer within a week; a fix goes into a patch release, and the advisory is published with credit to you unless you prefer otherwise.

## Supported versions

Only the latest release gets security fixes.

## Scope

In scope: the app and its installer, how it stores secrets and host keys, what it sends to servers, the release workflow. Out of scope: vulnerabilities in OpenSSH, WebView2 or Windows themselves, and attacks that need full control of the user's account on the computer already.
