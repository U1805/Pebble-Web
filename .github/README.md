<p align="center">
  <img src="../src/assets/app-icon.png" alt="Pebble logo" width="120">
</p>

<h1 align="center">Pebble Web</h1>

<p align="center">
  A local-first web email client for people who want a calmer, more private inbox.
</p>

<p align="center">
  <a href="README.zh-CN.md">简体中文</a>
  ·
  <a href="https://github.com/QingJ01/Pebble">Pebble Upstream</a>
  ·
  <a href="https://github.com/QingJ01/Pebble-Web">Official Pebble Web</a>
  ·
  <a href="../src-web/README.md">Web Documentation</a>
  ·
  <a href="../LICENSE">AGPL-3.0</a>
</p>

## Project Positioning

This repository is a long-term Web fork of [QingJ01/Pebble](https://github.com/QingJ01/Pebble).

- **Sole code upstream:** [QingJ01/Pebble](https://github.com/QingJ01/Pebble). Its core business logic, React pages, and desktop behavior are authoritative.
- **Official Web version:** [QingJ01/Pebble-Web](https://github.com/QingJ01/Pebble-Web), maintained by Pebble's original author, QingJ01.
- **Direction of this repository:** Maintain an independent Web adaptation layer directly on top of the current Pebble codebase, minimizing the long-term cost of merging upstream updates.

This project is not a reimplementation of Pebble, nor does it copy and maintain a separate set of core crates or a complete frontend.

For a complete overview of the Pebble desktop app, including screenshots and usage instructions, see the upstream [root README](../README.md) retained in this repository.

## Why Maintain This Fork?

Both Pebble Desktop and Pebble Web continue to evolve. If a Web project copies the core code and the entire frontend, their business behavior will gradually diverge, requiring the same migration and repair work to be repeated with every upgrade.

This project takes a different maintenance approach:

1. Directly use Pebble's current Rust core crates and React frontend.
2. Keep HTTP, WebSocket, browser API, and Web authentication code inside `src-web/`, with semantics aligned to the current Tauri implementation.
3. When upstream changes, accept the upstream business logic first, then update the Web adaptation.

## Current Capabilities

The current Web adaptation covers the primary personal email workflows, including:

- IMAP, POP3, Gmail OAuth, and Outlook OAuth accounts.
- Mail synchronization, folders, threads, search, and message state management.
- Sending through SMTP or OAuth, plus reply, reply all, forwarding, and drafts.
- Browser-based attachment uploads and downloads.
- Contacts, labels, trusted senders, and rules.
- Snooze, Kanban, and translation.
- Local backups, WebDAV, and diagnostic logs.
- Browser notifications and real-time WebSocket events.

Desktop concepts that do not exist in browsers—such as the system tray, launch at startup, native window controls, and setting the operating system's default email client—are not presented as successful business operations.

For desktop/Web differences and their implementations, see the [compatibility notes](../src-web/COMPATIBILITY.md). For architecture boundaries, upstream synchronization, and validation, see the [development guide](../src-web/DEVELOPMENT.md).

## Deployment

Download the Compose configuration:

```bash
curl -fsSLO https://raw.githubusercontent.com/U1805/Pebble-Web/web/src-web/docker-compose.yaml
```

Before starting, edit `docker-compose.yaml` and set at least `PEBBLE_PASSWORD` and `PEBBLE_JWT_SECRET` of 32 characters or more. OAuth environment variables are only needed when using Gmail or Outlook.

Start the service:

```bash
docker-compose up -d
```

Then open <http://localhost:8080> in a browser.

## Upstream Synchronization Principles

The repository maintains two long-lived branches with distinct purposes:

- `upstream`: follows `QingJ01/Pebble` through fast-forward updates.
- `web`: contains all Web adaptations.

Upstream updates are brought into `web` by merging `upstream`. When conflicts occur, the upstream business logic is retained by default and `src-web/` is adapted again; outdated business logic is not preserved merely to avoid conflicts.

Each upstream synchronization should pay particular attention to:

- Tauri command names, parameters, and return values.
- Tauri event names and payloads.
- Changes to mail, storage, OAuth, rules, and database migrations.
- Web service, shared frontend, desktop, and final deployment builds.

## Contributing

Issues and pull requests related to Web adaptation, upstream compatibility, and regression testing are welcome. Please observe the following boundaries when making changes:

- Check the current Pebble upstream implementation first; do not infer interfaces from old code.
- Do not copy the core crates or the complete frontend.
- Place Web-specific code in `src-web/` whenever possible.
- Keep command parameters, return values, persistence side effects, and event semantics aligned with Tauri.
- Keep each commit focused on one subject and add appropriate tests for behavioral changes.

## Acknowledgments and License

Thanks to [QingJ01](https://github.com/QingJ01) for creating and continuing to maintain Pebble and the official Pebble Web.

This project follows Pebble's [GNU Affero General Public License v3.0](../LICENSE).
