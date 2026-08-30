FeedLizard Linux is currently in beta. Please expect bugs and report anything
that does not work as expected.

## Highlights

- Native GTK4/libadwaita RSS, Atom, and JSON Feed reader.
- Local-first library, offline search, Scroll, and Pages reading modes.
- Improved article date recovery, local-time presentation, and deterministic
  chronological ordering when publishers omit or change date fields.
- Preserved publisher URL path semantics during OPML import, including feeds
  whose query endpoints require a trailing slash.
- Stabilized the three-pane reader layout and removed misleading broken-image
  placeholders when an article has no usable image.
- Improved the compact Omarchy companion with shared FeedLizard data,
  five-minute refresh, simultaneous full-app launching, and clearer timestamps.
- Architecture-specific Flatpak bundles for x86-64 and aarch64.

## Known validation limits

- Fedora aarch64 is the current native runtime-validation environment.
- Native x86-64 desktop testing remains required before 1.0.
- Omarchy runtime validation remains required on an actual Omarchy system.

Please report reproducible problems through GitHub Issues, without attaching
private subscription lists, databases, or Nostr keys.
