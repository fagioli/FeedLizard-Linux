# FeedLizard for Omarchy

An optional, lightweight FeedLizard bar widget for Omarchy's Quickshell-based
shell. It displays the unread count and a bounded folder summary, then delegates
Open, Open Unread, and Refresh to FeedLizard over a narrow session D-Bus API.
The widget's primary launch action explicitly requests FeedLizard's compact
Omarchy companion with `feedlizard --omarchy` (or the equivalent Flatpak
command). The full application and companion share one FeedLizard library.

## Requirements

- Omarchy quattro plugin architecture (`schemaVersion: 1`)
- Quickshell 0.3.x as supplied by current Omarchy
- FeedLizard Linux with integration protocol version 1
- `gdbus` and Python 3 from the normal Omarchy environment

## Installation

Install FeedLizard first. To run the companion directly without installing the
bar plugin:

```sh
flatpak run io.github.feedlizard.FeedLizard --omarchy
```

The plugin currently lives inside the FeedLizard Linux repository rather than a
standalone plugin repository. Clone the source, then use Omarchy's supported
local-plugin workflow:

```sh
git clone https://github.com/fagioli/FeedLizard-Linux.git
cd FeedLizard-Linux
omarchy plugin validate ./integrations/omarchy
omarchy plugin add ./integrations/omarchy --enable
```

Omarchy displays its normal warning because plugins execute as host-side shell
code. Review and approve it there. FeedLizard never writes Omarchy configuration
or bypasses this confirmation. A one-command remote install is not yet available
because Omarchy expects the plugin manifest at the root of its Git repository.

Update and removal are managed by Omarchy:

```sh
omarchy plugin update io.github.feedlizard.bar
omarchy plugin remove io.github.feedlizard.bar
```

## Security and privacy

The QML has no database or secret access. Its helper allows exactly four fixed
operations: read bounded unread state, open FeedLizard, open Unread, and request
FeedLizard's normal Refresh. It cannot execute caller-supplied commands through
FeedLizard. The plugin performs no network requests and stores no feed data.

When FeedLizard is stopped, the widget retains no stale count: it shows the icon
and Open FeedLizard remains available through the desktop entry.

## Testing status

Run deterministic source checks with `./tests/run`. Actual Omarchy installation,
panel rendering, themes, HiDPI, and shell stability require an Omarchy system.

The source and manifest were checked against the official Omarchy `quattro`
tree at commit `0ae1694830b6bd9511042fe1b89a0062d8c083cb` (August 25,
2026). The official `omarchy-plugin-validate` accepted the plugin, and the
Git-managed add, update, enable, disable, placement, and removal command paths
were exercised with disposable configuration and a simulated shell boundary.
Those checks do not substitute for rendering inside the real shell.

Omarchy's official installation and package repository are currently x86-64.
Do not treat an unofficial ARM port as release validation. On a supported
x86-64 Omarchy installation, run this acceptance sequence from a Git checkout:

```sh
omarchy plugin validate ./integrations/omarchy
omarchy plugin add ./integrations/omarchy --enable
omarchy plugin list
omarchy plugin disable io.github.feedlizard.bar
omarchy plugin enable io.github.feedlizard.bar --section right
omarchy plugin remove io.github.feedlizard.bar
```

Before removal, validate the bar count and panel with FeedLizard both stopped
and running; then validate Open FeedLizard, Open Unread, Refresh, live unread
changes, theme changes, scale factors, and shell stability. A published Git
remote is required to validate `omarchy plugin update` end to end.

**OMARCHY RUNTIME VALIDATION REQUIRED**
