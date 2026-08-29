FeedLizard Linux is currently in beta. Please expect bugs and report anything
that does not work as expected.

## Highlights

- Native GTK4/libadwaita RSS, Atom, and JSON Feed reader.
- Local-first library, offline search, Scroll, and Pages reading modes.
- Improved article dates, feed discovery, Reader extraction, image handling,
  navigation responsiveness, and contextual feed/article actions.
- Added bounded favicon failure backoff so blocked or broken publishers do not
  cause repeated image requests during a session.
- Added an optional compact Omarchy companion with shared FeedLizard data,
  keyboard controls, unread activity visualization, preview, and refresh.
- Added explicit `--omarchy` launch support and documented the confirmed
  Omarchy plugin installation workflow without weakening the Flatpak sandbox.
- Architecture-specific Flatpak bundles for x86-64 and aarch64.

## Known validation limits

- Fedora aarch64 is the current native runtime-validation environment.
- Native x86-64 desktop testing remains required before 1.0.
- Omarchy runtime validation remains required on an actual Omarchy system.

Please report reproducible problems through GitHub Issues, without attaching
private subscription lists, databases, or Nostr keys.
