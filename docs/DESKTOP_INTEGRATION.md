# FeedLizard desktop integration

FeedLizard exposes a deliberately small, read-oriented session D-Bus API for
optional desktop shell integrations. It is distribution-independent; Omarchy
is the first consumer, but no Omarchy code is linked into the RSS, storage,
networking, Reader, Nostr, or Lightning layers.

## D-Bus contract

- Bus name: `io.github.feedlizard.FeedLizard.Integration`
- Object: `/io/github/feedlizard/FeedLizard/Integration`
- Interface: `io.github.feedlizard.FeedLizard.Integration1`
- `GetUnreadState() -> string`: bounded JSON containing protocol version 1,
  total unread count, and at most five folders ordered by unread count.
- `OpenFeedLizard()`, `OpenUnread()`, and `Refresh()`: fixed application
  actions with no caller-controlled command, path, SQL, or object identifier.
- `UnreadChanged(string)`: event-driven signal carrying the same bounded JSON.

The API never returns feed URLs, articles, SQLite paths, OPML, Nostr data,
Lightning data, keys, or settings. The service reads through prepared storage
queries on a worker thread and does not block GTK. Rapid state changes are
coalesced through a capacity-one notification channel.

The Flatpak owns only the integration bus name; it does not receive general
session-bus access. If the service is unavailable, FeedLizard itself continues
normally and integrations show an icon without a count.

## Omarchy consumer

The separately removable source is in `../integrations/omarchy`. It targets the
Omarchy quattro schema-version-1 `bar-widget` plugin architecture and
Quickshell 0.3.x. QML never reads SQLite and performs no RSS, image, Nostr, or
network work. A persistent `gdbus monitor` observes service availability and
`UnreadChanged`; bounded state is fetched only at startup or after an event.

Installation after the plugin has an authorized public repository:

```sh
omarchy plugin add https://github.com/feedlizard/feedlizard-omarchy.git --enable
```

Omarchy presents its normal unsandboxed-plugin warning and confirmation. Git
installations update with `omarchy plugin update io.github.feedlizard.bar` and
are removed with `omarchy plugin remove io.github.feedlizard.bar`. FeedLizard
does not copy files into `~/.config/omarchy` or implement an updater.

The repository URL above is the intended publication location, not a claim
that it has already been published. Official builds should set
`FEEDLIZARD_OMARCHY_PLUGIN_REPOSITORY` only after that repository exists.

## Detection and status

The GTK Settings integration row appears only when the inherited environment
contains an absolute `OMARCHY_PATH`, or `XDG_CURRENT_DESKTOP` explicitly names
Omarchy. Detection is local, one-shot, and causes no network or filesystem
probe. Non-Omarchy systems load no QML and start no Omarchy process.

The manifest and helper can be validated on any Linux system. On August 26,
2026, the official validator from Omarchy `quattro` commit
`0ae1694830b6bd9511042fe1b89a0062d8c083cb` accepted the plugin. Its current
schema-version-1 manifest, Git installation lifecycle, and first-party QML
component contracts were also checked. Disposable add/update/remove and direct
enable/disable/placement command tests passed without writing the user's real
Omarchy configuration.

The official Omarchy installation and repository currently target x86-64, so
an unofficial ARM port is not an acceptable release-validation environment.
Visual behavior, installation confirmation, rendering, live D-Bus updates,
theme integration, HiDPI, and shell stability still require an actual
supported x86-64 Omarchy session. The complete command and interaction
checklist is maintained in `../integrations/omarchy/README.md`.

**OMARCHY RUNTIME VALIDATION REQUIRED**
