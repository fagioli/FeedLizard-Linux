# FeedLizard for Omarchy

FeedLizard's Omarchy experience is a compact companion over the production
FeedLizard engine. It is not a second RSS reader.

## Launch contract

- `feedlizard` opens the complete GTK/libadwaita application.
- `feedlizard --omarchy` opens the compact FeedLizard Omarchy companion.
- `FEEDLIZARD_OMARCHY=1 feedlizard` is the explicit fallback.
- The command-line argument takes priority and is removed before GTK parses
  application options.

The companion uses a non-unique GTK application instance, so it can appear even
when the full FeedLizard application is already running. `OPEN FEEDLIZARD`
starts the same executable without the Omarchy flag; normal application
activation then opens or focuses the complete reader.

Omarchy detection never changes the runtime mode. Only an explicit launch from
the plugin selects the companion.

## Shared engine and data

Both presentations use the same application ID, database path, SQLite schema,
storage worker, refresh coordinator, parser, identities, and read/star
mutations. The companion neither owns nor copies subscriptions or articles.
Closing one presentation and opening the other shows the same library.

The companion displays a bounded latest-unread article list, real unread/feed
counts, and real refresh status. It supports navigation, preview, read/unread,
star, open-original, refresh, and opening the full application. Feed management,
folders, OPML, Settings, Nostr, and substantial reading remain in FeedLizard.

## Discovery in the Flatpak

Normal FeedLizard shows the Omarchy integration row only when the exact current
production marker `OMARCHY_PATH=/usr/share/omarchy` is present. Omarchy exports
this variable and Flatpak forwards it into the sandbox. Arch, Hyprland, Wayland,
themes, host files, and package presence are deliberately not treated as proof.

The forwarding behavior was exercised in the Fedora aarch64 reference VM using
the release Flatpak: an inherited `OMARCHY_PATH=/usr/share/omarchy` remained
visible inside the sandbox without adding a Flatpak permission or override.

Detection is discovery only. It never installs, enables, or launches anything.

## Installation security boundary

Current Omarchy plugins are Git repositories installed by `omarchy plugin add
<repository> --enable`. Omarchy deliberately displays a warning and confirmation
because plugins execute host-side shell code.

A Flatpak cannot run that host installer without the broad
`org.freedesktop.Flatpak` HostCommand permission. FeedLizard does not request
that permission. Settings therefore copies the supported install command after
the official standalone plugin repository is configured; the user runs it on
the host and approves Omarchy's normal confirmation. Until that repository is
published, Settings states that installation is not yet available.

The same boundary means the sandbox cannot truthfully inspect or remove the
host checkout in `~/.config/omarchy/plugins/`. Installed/update/remove state is
therefore not guessed. Omarchy remains the authority through `omarchy plugin
list`, `omarchy plugin update`, and `omarchy plugin remove`.

## Plugin boundary

The Quickshell bar widget reads bounded unread state and invokes four fixed
actions over FeedLizard's narrow session D-Bus interface. It never reads SQLite,
RSS bodies, Nostr material, or Secret Service. Its primary action invokes the
explicit compact mode using the Flatpak or native executable.

Actual Omarchy/Quickshell rendering and lifecycle validation remains distinct
from Fedora GTK development validation.
