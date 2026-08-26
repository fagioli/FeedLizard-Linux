# Compatibility fixtures

These inputs capture behavior that Linux must keep compatible with the Apple
implementation in `FeedCore.swift`, `FeedParser.swift`, and `StableIdentity.swift`.

The initial corpus covers RSS and Atom feed metadata, article identity
precedence, missing-title behavior, entity decoding, authors, publication
timestamps, and URL normalization. Add regression fixtures here before changing
portable behavior.

`identity.tsv` contains fixed outputs calculated independently from the Apple
v1 normalization and SHA-256 contract. These values must not be regenerated just
because an implementation changes.

Phase 2 adds synthetic RDF, JSON Feed, malformed-but-usable, and OPML inputs.
The identity table also fixes the requested BleepingComputer feed identity.

`nostr-history.json` defines the portable historical-snapshot event model,
multi-relay duplicate topology, newest-five policy, and equal-timestamp ordering
rule. Tests create signed encrypted synthetic events from it; it contains no
real user key or subscription.
